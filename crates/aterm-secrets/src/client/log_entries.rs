// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Log-backed secret entries and 0.145 envelope mirroring (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §3.3.6, §3.5.3, F14).

use super::*;
use crate::commons::bootstrap::Device;
use crate::membership_log::{
    Body, Entrant, EpochAudience, Evidence, Log, MemberId, Record, RecordId, Root, Store,
    encode_records,
};
use aterm_digest::Sha256;

const HEADS_DIGEST_MAGIC: &[u8; 5] = b"ALHD1";

/// Deterministic SHA-256 digest over a [`Log`]'s current head [`RecordId`]s.
pub(super) fn records_heads_digest(log: &Log) -> [u8; 32] {
    let heads = log.heads();
    let mut buf = Vec::with_capacity(HEADS_DIGEST_MAGIC.len() + heads.len() * 32);
    buf.extend_from_slice(HEADS_DIGEST_MAGIC);
    for id in heads {
        buf.extend_from_slice(&id);
    }
    Sha256::digest(&buf)
}

pub(super) fn genesis_matches_lineage(
    log: &Log,
    config: &Config,
    envelope_signer: &[u8; 32],
    handoff: &[([u8; 32], [u8; 64])],
) -> bool {
    let Body::Genesis { signer, .. } = log.genesis().body() else {
        return false;
    };
    signer == envelope_signer
        || config.signer.as_ref() == Some(signer)
        || config.followed.contains(signer)
        || handoff.iter().any(|(key, _)| key == signer)
}

pub(super) fn trusted_seeded_store(
    paths: &Paths,
    config: &Config,
    envelope_signer: &[u8; 32],
    handoff: &[([u8; 32], [u8; 64])],
) -> crate::Result<Option<(Store, [u8; 32])>> {
    let store = super::load_records(paths)?;
    let Some(log) = store.log() else {
        return Ok(None);
    };
    if !log.has_epochs() || !genesis_matches_lineage(log, config, envelope_signer, handoff) {
        return Ok(None);
    }
    let digest = records_heads_digest(log);
    Ok(Some((store, digest)))
}

pub(super) fn trusted_seeded_log(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
) -> crate::Result<Option<(Log, [u8; 32])>> {
    let Some((store, digest)) =
        trusted_seeded_store(paths, config, &envelope.signer, &envelope.handoff)?
    else {
        return Ok(None);
    };
    Ok(store.log().cloned().map(|log| (log, digest)))
}

pub(super) struct ConfiguredLog {
    pub(super) log: Log,
}

pub(super) fn trusted_configured_log(
    paths: &Paths,
    config: &Config,
) -> crate::Result<Option<ConfiguredLog>> {
    let Some(pin) = config.signer else {
        return Ok(None);
    };
    let store = super::load_records(paths)?;
    let Some(log) = store.log() else {
        return Ok(None);
    };
    if !log.has_epochs() || !genesis_matches_lineage(log, config, &pin, &[]) {
        return Ok(None);
    }
    if !matches!(log.genesis().body(), Body::Genesis { .. }) {
        return Ok(None);
    }
    Ok(Some(ConfiguredLog { log: log.clone() }))
}

pub(super) fn trusted_local_log(paths: &Paths, config: &Config) -> crate::Result<Option<Log>> {
    let Some(_observation) = super::StateLock::observe(paths)? else {
        return Ok(None);
    };
    let state = super::load_state(paths)?;
    if state.membership != super::Stored::In || state.failed || !state.pending.is_empty() {
        return Ok(None);
    }
    if state.digest.is_none() && super::fsx::kind(&paths.cache_file())? == super::fsx::Kind::Missing
    {
        if state.source_identity != Some(super::source_identity(config))
            || !super::pin_admitted(config, None)
        {
            return Ok(None);
        }
        return Ok(trusted_configured_log(paths, config)?.map(|c| c.log));
    }
    if !super::source_bound(config, &state) || !super::pin_admitted(config, None) {
        return Ok(None);
    }
    let Some(cached) = super::held_copy(paths, config, &state) else {
        return Ok(None);
    };
    let envelope = super::envelope::parse(&cached)?;
    Ok(trusted_seeded_log(paths, config, &envelope)?.map(|(log, _)| log))
}

/// Configure a newcomer's local vault source, signer pin, and lock state from a pinned source
/// spec (`<url-or-path>#<signer>` or ALab default URL) so log-backed Commons and secret readers
/// operate from `<name>.records` without requiring an admin-signed `0.145` envelope (`F18`).
///
/// # Errors
/// Invalid host or source specification, missing signer pin, or local file I/O failure.
pub fn configure_log_member(paths: &Paths, source_spec: &str, host: &str) -> crate::Result<()> {
    if !text::valid_machine_name(host) {
        return Err(Error::new(format!("{host:?} is not a valid machine name")));
    }
    let typed = source_spec.trim();
    let typed = if typed.is_empty() {
        crate::ALAB_DEFAULT_URL
    } else {
        typed
    };
    let (source, fragment) = transport::parse_source(typed, &paths.home)?;
    let name = transport::vault_name(&source)?;
    let signer = match fragment {
        Some(pin) => pin,
        None if crate::is_alab_default_url(typed) && super::alab_signer_built_in() => {
            decode_key32(crate::ALAB_SIGNER)
                .ok_or_else(|| Error::new("invalid built-in ALab signer"))?
        }
        None => {
            return Err(Error::new(
                "log-backed member configuration requires a pinned signer",
            ));
        }
    };
    let config = Config {
        source: source.clone(),
        name,
        signer: Some(signer),
        followed: Vec::new(),
        host: host.to_owned(),
    };
    let _lock = StateLock::take(paths)?;
    let _ = machine_identity(paths)?;
    let mut state = load_state(paths)?;
    state.source_identity = Some(super::source_identity(&config));
    state.membership = Stored::In;
    save_state(paths, &state)?;
    save_config(paths, &config)
}

pub(super) fn current_records_digest(
    paths: &Paths,
    config: &Config,
    state: &State,
) -> Option<[u8; 32]> {
    if state.digest.is_none()
        && super::fsx::kind(&paths.cache_file()).ok() == Some(super::fsx::Kind::Missing)
    {
        return trusted_configured_log(paths, config)
            .ok()
            .flatten()
            .map(|c| records_heads_digest(&c.log));
    }
    let cached = super::held_copy(paths, config, state).or_else(|| {
        fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
            .ok()
            .flatten()
            .map(|b| b.as_slice().to_vec())
    })?;
    let env = envelope::parse(&cached).ok()?;
    trusted_seeded_log(paths, config, &env)
        .ok()
        .flatten()
        .map(|(_, d)| d)
}

/// Whether this Mac reads its entries from the seeded `log` rather than from `envelope`'s
/// lockers: once the log holds a Wrap addressed to it, or removed it. A Mac an admin let in
/// through the envelope alone (an admin still on 0.145, or one with no seat in the log) has no
/// Wrap until a seated admin closes the gap, and reads its lockers until then: the log names it
/// neither in nor out.
pub(super) fn reads_log(log: &Log, identity: &Identity) -> bool {
    let me = identity.recipient();
    log.wraps_to(&me)
        || log.removes(&me)
        || log.audience_left_out_recipient(&EpochAudience::Everyone, &me)
}

/// [`trusted_seeded_log`], where this Mac reads `envelope`'s entries from it: the log holds every
/// record the envelope's entries were written at ([`LogBase::Covers`]) and this Mac
/// [`reads_log`] it. A log behind the envelope, or an envelope a writer with no seat in the log
/// published, is read from its lockers instead, so an older log never stands in for a newer
/// version.
pub(super) fn readable_log(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    identity: &Identity,
) -> crate::Result<Option<(Log, [u8; 32])>> {
    let Some((log, digest)) = trusted_seeded_log(paths, config, envelope)? else {
        return Ok(None);
    };
    if log_base(Some(&log), envelope, identity)? != LogBase::Covers {
        return Ok(None);
    }
    Ok(reads_log(&log, identity).then_some((log, digest)))
}

/// The comment a publish whose writer's log holds its entries puts second in every locker but the
/// admin one: the membership log's heads once the envelope's entries were written to it. Every
/// reader skips comment lines, a `0.145` Mac's included, so it changes nothing an older Mac reads.
/// A publish by a writer with no seat in the log, or whose log is behind the version it edits,
/// carries none.
const STAMP_PREFIX: &str = "# log-heads";
/// The most heads a stamp names: a log with more heads stamps nothing, and its envelope is read
/// as one a writer with no seat published.
const MAX_STAMP_HEADS: usize = 32;

/// The stamp naming `heads`: `None` for none, or more than [`MAX_STAMP_HEADS`].
pub(super) fn stamp_line(heads: &[RecordId]) -> Option<String> {
    if heads.is_empty() || heads.len() > MAX_STAMP_HEADS {
        return None;
    }
    let mut line = String::from(STAMP_PREFIX);
    for head in heads {
        line.push(' ');
        aterm_codec::hex::push_lower(&mut line, head);
    }
    Some(line)
}

/// A locker's `text` with `stamp` after its first line (its section header).
pub(super) fn stamped_text(text: &[u8], stamp: &str) -> zeroize::Zeroizing<Vec<u8>> {
    let cut = text
        .iter()
        .position(|&b| b == b'\n')
        .map_or(text.len(), |at| at + 1);
    let mut out = zeroize::Zeroizing::new(Vec::with_capacity(text.len() + stamp.len() + 2));
    out.extend_from_slice(&text[..cut]);
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(stamp.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&text[cut..]);
    out
}

fn parse_stamp(plain: &[u8]) -> Option<Vec<RecordId>> {
    let mut lines = plain.split(|&b| b == b'\n');
    let _header = lines.next()?;
    let line = std::str::from_utf8(lines.next()?).ok()?;
    let rest = line.strip_prefix(STAMP_PREFIX)?;
    let mut heads = Vec::new();
    for word in rest.split_ascii_whitespace() {
        let mut id = [0u8; 32];
        if word.len() != 64 || !aterm_codec::hex::decode_into(word.as_bytes(), &mut id) {
            return None;
        }
        heads.push(id);
    }
    (!heads.is_empty() && heads.len() <= MAX_STAMP_HEADS).then_some(heads)
}

/// What `envelope` says to one Mac of the log its entries were written at.
enum Stamp {
    /// No locker but the admin one is addressed to it: the envelope holds no version for it.
    NoLocker,
    /// Its locker carries no stamp.
    Unstamped,
    /// Its locker's stamp names these heads.
    Heads(Vec<RecordId>),
}

/// The heads `envelope`'s stamp names ([`STAMP_PREFIX`]), read from the first locker but the
/// admin one that `identity` opens.
///
/// # Errors
/// A locker addressed to `identity` does not decrypt.
fn envelope_stamp(envelope: &Envelope, identity: &Identity) -> crate::Result<Stamp> {
    for locker in envelope.lockers.iter().filter(|l| l.label != "admin") {
        match age::decrypt(std::slice::from_ref(identity), locker.armored.as_bytes()) {
            Ok(plain) => {
                return Ok(parse_stamp(plain.as_slice()).map_or(Stamp::Unstamped, Stamp::Heads));
            }
            Err(age::AgeError::NoMatch) => {}
            Err(error) => return Err(Error::new(format!("locker {}: {error}", locker.label))),
        }
    }
    Ok(Stamp::NoLocker)
}

/// Whether `envelope` holds a locker but the admin one addressed to `identity`: an admin's
/// publish names this Mac in `[machines]`.
///
/// # Errors
/// A locker addressed to `identity` does not decrypt.
pub(super) fn addressed_to(envelope: &Envelope, identity: &Identity) -> crate::Result<bool> {
    Ok(!matches!(
        envelope_stamp(envelope, identity)?,
        Stamp::NoLocker
    ))
}

/// Whether `log` alone holds `identity`'s version: the log lets it in, and no newest `[everyone]`
/// epoch leaves it out on purpose ([`Log::left_out`]). A Mac a member admitted into the log
/// (F15) has no envelope locker, and reads the log; one an admin's rotation left out does not.
pub(super) fn log_alone_admits(log: &Log, identity: &Identity) -> crate::Result<bool> {
    let me = identity.recipient();
    let membership = log.membership();
    let Some((actor, _)) = membership
        .members()
        .iter()
        .find(|(_, member)| member.binding.recipient() == me)
    else {
        return Ok(false);
    };
    let Some(opened) = log.open_audience_entries(identity)? else {
        return Ok(false);
    };
    Ok(!log.left_out(&opened, &EpochAudience::Everyone, actor))
}

/// How a seeded log stands to an envelope: whether it holds every record the envelope's entries
/// were written at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LogBase {
    /// No seeded log of this vault's lineage here.
    NoLog,
    /// The envelope names no stamp: its writer had no seat in the log, or predates stamps.
    Unstamped,
    /// The log lacks a record the envelope's entries were written at.
    Behind,
    /// The log holds every record the envelope's entries were written at.
    Covers,
}

/// How `log` (when there is one) stands to `envelope`, as `identity` reads its stamp.
///
/// # Errors
/// A locker addressed to `identity` does not decrypt.
pub(super) fn log_base(
    log: Option<&Log>,
    envelope: &Envelope,
    identity: &Identity,
) -> crate::Result<LogBase> {
    let Some(log) = log else {
        return Ok(LogBase::NoLog);
    };
    Ok(match envelope_stamp(envelope, identity)? {
        Stamp::NoLocker
            if log_alone_admits(log, identity)?
                || log.removes(&identity.recipient())
                || log.audience_left_out_recipient(
                    &EpochAudience::Everyone,
                    &identity.recipient(),
                ) =>
        {
            LogBase::Covers
        }
        Stamp::NoLocker | Stamp::Unstamped => LogBase::Unstamped,
        Stamp::Heads(heads) if heads.iter().all(|id| log.get(id).is_some()) => LogBase::Covers,
        Stamp::Heads(_) => LogBase::Behind,
    })
}

pub(super) type OpenedWithDigest = (Option<Vec<Entry>>, Option<[u8; 32]>);

pub(super) fn open_entries_preferring_log(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    identity: &Identity,
) -> crate::Result<OpenedWithDigest> {
    if let Some((log, digest)) = trusted_seeded_log(paths, config, envelope)?
        && (log.removes(&identity.recipient())
            || (log_base(Some(&log), envelope, identity)? != LogBase::Behind
                && log
                    .audience_left_out_recipient(&EpochAudience::Everyone, &identity.recipient())))
    {
        return Ok((None, Some(digest)));
    }
    if let Some((log, digest)) = readable_log(paths, config, envelope, identity)? {
        return Ok((log.open_entries(identity)?, Some(digest)));
    }
    Ok((super::open_lockers(envelope, identity)?, None))
}

fn same_entry(a: &Entry, b: &Entry) -> bool {
    a.name == b.name
        && a.audience == b.audience
        && a.if_absent == b.if_absent
        && a.anchor == b.anchor
        && aterm_digest::ct_eq(a.value.as_slice(), b.value.as_slice())
}

/// Seed a fresh [`Log`] (holding [`Body::Genesis`]) with an [`Body::Epoch`], [`Body::Wrap`]s and
/// [`Body::Put`]s for every locker and entry in `doc`, plus [`EpochAudience::Commons`] when `doc`
/// carries `aterm-commons-control-v1`.
pub(super) fn seed_log_from_document_with_commons_generation(
    log: &mut Log,
    doc: &Document,
    actor: &MemberId,
    writer_seed: &[u8; 32],
    incarnation: [u8; 16],
    authored_ms: u64,
    commons_generation: u64,
) -> crate::Result<Vec<Record>> {
    if !log.membership().members().contains_key(actor) {
        return Err(Error::new(
            "seeding membership log entries requires an active founding member",
        ));
    }
    let commons_entry = doc
        .entries()
        .into_iter()
        .find(|e| e.name == crate::commons::ENTRY);
    let mut entries_by_aud: BTreeMap<Audience, Vec<Entry>> = BTreeMap::new();
    for entry in doc.entries() {
        if entry.name == crate::commons::ENTRY {
            continue;
        }
        entries_by_aud
            .entry(entry.audience.clone())
            .or_default()
            .push(entry);
    }
    let mut minted = Vec::new();
    for plan in doc.lockers() {
        let mut recipients = Vec::with_capacity(plan.recipients.len());
        for text in &plan.recipients {
            recipients.push(Recipient::parse(text).map_err(|_| Error::new("bad machine key"))?);
        }
        if recipients.is_empty() {
            continue;
        }
        let entries_for_plan = entries_by_aud
            .get(&plan.audience)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if matches!(plan.audience, Audience::Hosts(_)) && entries_for_plan.is_empty() {
            continue;
        }
        let (draft_ep, epoch_key) =
            log.draft_epoch(actor, authored_ms, plan.audience.clone(), 1)?;
        let ep_rec = draft_ep.sign(writer_seed, incarnation)?;
        let epoch_id = *ep_rec.id();
        let _ = log.insert(ep_rec.clone())?;
        minted.push(ep_rec);

        for recipient in recipients {
            let wrap_rec = log
                .draft_wrap(actor, authored_ms, epoch_id, &epoch_key, recipient)?
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(wrap_rec.clone())?;
            minted.push(wrap_rec);
        }
        for entry in entries_for_plan {
            let put_rec = log
                .draft_put(actor, authored_ms, epoch_id, &epoch_key, entry)?
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(put_rec.clone())?;
            minted.push(put_rec);
        }
    }

    if let Some(control) = commons_entry {
        let mut founder_recipients = BTreeSet::new();
        for member in log.membership().members().values() {
            founder_recipients.insert(member.binding.recipient());
        }
        if let Audience::Hosts(hosts) = &control.audience {
            let by_name: BTreeMap<String, Recipient> = doc
                .machines()
                .into_iter()
                .filter_map(|m| Recipient::parse(&m.recipient).ok().map(|r| (m.name, r)))
                .collect();
            for host in hosts {
                if let Some(&r) = by_name.get(host)
                    && !log.recipient_is_removed(&r)
                {
                    founder_recipients.insert(r);
                }
            }
        }
        if !founder_recipients.is_empty() {
            let (draft_ep, epoch_key) = log.draft_epoch(
                actor,
                authored_ms,
                EpochAudience::Commons,
                commons_generation.max(1),
            )?;
            let ep_rec = draft_ep.sign(writer_seed, incarnation)?;
            let epoch_id = *ep_rec.id();
            let _ = log.insert(ep_rec.clone())?;
            minted.push(ep_rec);

            for recipient in founder_recipients {
                let wrap_rec = log
                    .draft_wrap(actor, authored_ms, epoch_id, &epoch_key, recipient)?
                    .sign(writer_seed, incarnation)?;
                let _ = log.insert(wrap_rec.clone())?;
                minted.push(wrap_rec);
            }

            let put_rec = log
                .draft_commons_put(
                    actor,
                    authored_ms,
                    epoch_id,
                    &epoch_key,
                    control.value.as_slice(),
                )?
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(put_rec.clone())?;
            minted.push(put_rec);
        }
    }
    Ok(minted)
}

/// Overlay the winning entries from `log` across every [`Audience`] fully unwrapped by `identity`
/// onto `doc`, leaving audiences `identity` cannot unwrap untouched. Returns whether `doc` changed.
pub(super) fn overlay_log_onto_document(
    log: &Log,
    identity: &Identity,
    doc: &mut Document,
) -> crate::Result<bool> {
    let Some(opened) = log.open_audience_entries(identity)? else {
        return Ok(false);
    };
    let mut changed = false;

    let initial_doc_recipients: BTreeSet<Recipient> = doc
        .machines()
        .into_iter()
        .filter_map(|m| Recipient::parse(&m.recipient).ok())
        .collect();
    if log.has_commons_epochs() {
        let mut admins: BTreeSet<String> = doc.admins().into_iter().collect();
        for machine in doc.machines() {
            if admins.contains(&machine.name) && admins.len() <= 1 {
                continue;
            }
            if let Ok(recipient) = Recipient::parse(&machine.recipient)
                && log.recipient_is_removed(&recipient)
                && doc.remove_machine(&machine.name, true) == Ok(true)
            {
                admins.remove(&machine.name);
                changed = true;
            }
        }
        let code_only: BTreeSet<String> = doc.code_only().into_iter().collect();
        let mut existing_recipients: BTreeSet<Recipient> = doc
            .machines()
            .into_iter()
            .filter_map(|m| Recipient::parse(&m.recipient).ok())
            .collect();
        let mut existing_names: BTreeSet<String> =
            doc.machines().into_iter().map(|m| m.name).collect();
        for (actor, member) in log.membership().members() {
            let recipient = member.binding.recipient();
            if existing_recipients.contains(&recipient)
                || code_only.contains(&recipient.to_bech32())
                || log.left_out(&opened, &EpochAudience::Everyone, actor)
            {
                continue;
            }
            let candidate = if text::valid_machine_name(&member.name)
                && !existing_names.contains(&member.name)
            {
                member.name.clone()
            } else if text::valid_machine_name(&member.label)
                && !existing_names.contains(&member.label)
            {
                member.label.clone()
            } else {
                let d = Sha256::digest(member.binding.bytes());
                format!("{}-{:02x}{:02x}", member.label, d[0], d[1])
            };
            if text::valid_machine_name(&candidate)
                && doc.add_machine(&candidate, &recipient.to_bech32()).is_ok()
            {
                existing_recipients.insert(recipient);
                existing_names.insert(candidate);
                changed = true;
            }
        }
    }

    if let Some(commons_ctrl) = &opened.commons_control
        && opened
            .fully_unwrapped_audiences
            .contains(&EpochAudience::Commons)
    {
        let cur_commons = doc
            .entries()
            .into_iter()
            .find(|e| e.name == crate::commons::ENTRY);
        let cur_host_set: BTreeSet<String> = match cur_commons.as_ref().map(|e| &e.audience) {
            Some(Audience::Hosts(hosts)) => hosts.iter().cloned().collect(),
            _ => BTreeSet::new(),
        };
        let code_only: BTreeSet<String> = doc.code_only().into_iter().collect();
        let membership = log.membership();
        let log_commons_recipients: BTreeSet<Recipient> = membership
            .members()
            .iter()
            .filter(|(actor, _)| !log.left_out(&opened, &EpochAudience::Commons, actor))
            .map(|(_, member)| member.binding.recipient())
            .filter(|r| {
                !code_only.contains(&r.to_bech32())
                    && (!initial_doc_recipients.contains(r)
                        || log.wraps_maximal_audience_to(&EpochAudience::Commons, r))
            })
            .collect();
        let target_hosts: Vec<String> = doc
            .machines()
            .into_iter()
            .filter(|m| {
                let Ok(r) = Recipient::parse(&m.recipient) else {
                    return false;
                };
                !log.recipient_is_removed(&r)
                    && (cur_host_set.contains(&m.name) || log_commons_recipients.contains(&r))
            })
            .map(|m| m.name)
            .collect();
        if !target_hosts.is_empty() {
            let target_aud = Audience::Hosts(target_hosts);
            let needs_update = cur_commons.as_ref().is_none_or(|cur| {
                cur.audience != target_aud
                    || !aterm_digest::ct_eq(cur.value.as_slice(), commons_ctrl.as_slice())
            });
            if needs_update {
                doc.set_entry(&target_aud, crate::commons::ENTRY, commons_ctrl.as_slice())
                    .map_err(|e| Error::new(e.to_string()))?;
                changed = true;
            }
        }
    }

    if opened.fully_unwrapped_audiences.is_empty() {
        return Ok(changed);
    }
    let known_machines: BTreeSet<String> = doc.machines().into_iter().map(|m| m.name).collect();
    let eligible_audience = |aud: &Audience| match aud {
        Audience::Everyone => !known_machines.is_empty(),
        Audience::Hosts(hosts) => hosts.iter().all(|h| known_machines.contains(h)),
    };

    let mut target_host_audience: BTreeMap<String, Audience> = BTreeMap::new();
    for (name, win_aud) in &opened.winning_host_audience {
        if opened
            .fully_unwrapped_audiences
            .contains(&EpochAudience::from(win_aud))
            && eligible_audience(win_aud)
        {
            target_host_audience.insert(name.clone(), win_aud.clone());
        } else if let Some(fallback_aud) = opened
            .fully_unwrapped_audiences
            .iter()
            .filter_map(EpochAudience::as_text_audience)
            .filter(|aud| matches!(aud, Audience::Hosts(_)) && eligible_audience(aud))
            .find(|aud| {
                opened
                    .by_audience
                    .get(aud)
                    .is_some_and(|m| m.contains_key(name))
            })
        {
            target_host_audience.insert(name.clone(), fallback_aud);
        }
    }

    // 1. Remove entries deleted in fully-unwrapped audiences (anchored entries first so anchor
    //    dependencies never dangle mid-removal).
    let existing = doc.entries();
    let doc_has_commons = existing.iter().any(|e| e.name == crate::commons::ENTRY);
    for want_anchored in [true, false] {
        for cur in &existing {
            if cur.anchor.is_some() != want_anchored {
                continue;
            }
            if !opened
                .fully_unwrapped_audiences
                .contains(&EpochAudience::from(&cur.audience))
                || !eligible_audience(&cur.audience)
            {
                continue;
            }
            // Preserve `doc`'s authoritative Commons control descriptor whenever `doc` already
            // carries one (it is signed by `commons_seed` and reconciled with `admissions` across
            // branches, then mirrored back to the log by `append_document_diff_to_log`).
            if cur.name == crate::commons::ENTRY && doc_has_commons {
                continue;
            }
            let keep = match &cur.audience {
                Audience::Everyone => opened
                    .by_audience
                    .get(&cur.audience)
                    .is_some_and(|m| m.contains_key(&cur.name)),
                Audience::Hosts(_) => {
                    target_host_audience.get(&cur.name) == Some(&cur.audience)
                        && opened
                            .by_audience
                            .get(&cur.audience)
                            .is_some_and(|m| m.contains_key(&cur.name))
                }
            };
            if !keep
                && doc
                    .unset_entry(&cur.audience, &cur.name)
                    .map_err(|e| Error::new(e.to_string()))?
            {
                changed = true;
            }
        }
    }

    // 2. Apply live winning entries from fully-unwrapped audiences (unanchored entries first so
    //    anchors exist before `if-absent=<anchor>` entries reference them).
    for want_anchored in [false, true] {
        for ep_aud in &opened.fully_unwrapped_audiences {
            let Some(aud) = ep_aud.as_text_audience() else {
                continue;
            };
            if !eligible_audience(&aud) {
                continue;
            }
            let Some(map) = opened.by_audience.get(&aud) else {
                continue;
            };
            for entry in map.values() {
                if entry.anchor.is_some() != want_anchored {
                    continue;
                }
                if entry.name == crate::commons::ENTRY && doc_has_commons {
                    continue;
                }
                if matches!(aud, Audience::Hosts(_))
                    && target_host_audience.get(&entry.name) != Some(&aud)
                {
                    continue;
                }
                if let Some(cur) = doc.entry(&aud, &entry.name)
                    && same_entry(cur, entry)
                {
                    continue;
                }
                if let Audience::Hosts(hosts) = &aud {
                    for prior in doc.entries() {
                        if prior.name == entry.name
                            && prior.audience != aud
                            && let Audience::Hosts(other_hosts) = &prior.audience
                            && other_hosts.iter().any(|h| hosts.contains(h))
                        {
                            let _ = doc
                                .unset_entry(&prior.audience, &entry.name)
                                .map_err(|e| Error::new(e.to_string()))?;
                        }
                    }
                }
                doc.set_entry_with(
                    &aud,
                    &entry.name,
                    entry.value.as_slice(),
                    entry.if_absent,
                    entry.anchor.as_deref(),
                )
                .map_err(|e| Error::new(e.to_string()))?;
                changed = true;
            }
        }
    }

    Ok(changed)
}

pub(super) fn open_local_member_signer(
    paths: &Paths,
    log: &Log,
) -> crate::Result<Option<(MemberId, Device, [u8; 16])>> {
    if fsx::kind(&paths.dir.join("commons").join("device.age"))? != fsx::Kind::File {
        return Ok(None);
    }
    let device = match Device::open_existing(paths) {
        Ok(device) => device,
        Err(_) => return Ok(None),
    };
    let Body::Genesis { root, .. } = log.genesis().body() else {
        return Ok(None);
    };
    let req = device.request(root.network, root.authority)?;
    let actor = req.binding().actor();
    let incarnation = *req.binding().incarnation();
    let membership = log.membership();
    let Some(member) = membership.members().get(&actor) else {
        return Ok(None);
    };
    if &member.binding != req.binding() || log.audience_left_out(&EpochAudience::Everyone, &actor) {
        return Ok(None);
    }
    Ok(Some((actor, device, incarnation)))
}

/// The age recipients of `membership`'s members, less the keys `doc` leaves to approval by code
/// (`[join]`'s `code-only` lines: a machine an admin let go). Any member can admit a Mac into the
/// log (F15), so a member the log holds but `[machines]` does not name is one an admin never
/// saw, not one it revoked.
fn log_member_recipients(
    membership: &crate::membership_log::Membership,
    doc: &Document,
) -> BTreeSet<Recipient> {
    let code_only: BTreeSet<String> = doc.code_only().into_iter().collect();
    membership
        .members()
        .values()
        .map(|member| member.binding.recipient())
        .filter(|recipient| !code_only.contains(&recipient.to_bech32()))
        .collect()
}

fn pending_remove_cutoffs(
    log: &Log,
    actor: &MemberId,
    matches_recipient: impl Fn(&Recipient) -> bool,
) -> Vec<(MemberId, RecordId)> {
    let membership = log.membership();
    let voided = membership.voided();
    let mut out = Vec::new();
    for (id, m) in membership.members() {
        if id == actor || !matches_recipient(&m.binding.recipient()) {
            continue;
        }
        let cutoff = log
            .records()
            .skip(1)
            .filter(|r| !voided.contains(r.id()) && r.actor() == id)
            .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id())))
            .map_or(m.entry, |r| *r.id());
        out.push((*id, cutoff));
    }
    let mut removed_matching: BTreeSet<MemberId> = BTreeSet::new();
    if let Body::Genesis { founders, .. } = log.genesis().body() {
        for f in founders {
            if let Some(b) = &f.binding
                && matches_recipient(&b.recipient())
            {
                removed_matching.insert(b.actor());
            }
        }
    }
    for r in log.records().skip(1) {
        if voided.contains(r.id()) {
            continue;
        }
        if let Body::Admit { entrant, .. } = r.body()
            && r.actor() != &entrant.member()
            && matches_recipient(&entrant.request.binding().recipient())
        {
            removed_matching.insert(entrant.member());
        }
    }
    for (target, removal) in membership.removed() {
        if target == actor
            || removal.quorum
            || removal.by.contains(actor)
            || !removed_matching.contains(target)
        {
            continue;
        }
        let mut by_cutoff: BTreeMap<RecordId, BTreeSet<MemberId>> = BTreeMap::new();
        for r in log.records().skip(1) {
            if voided.contains(r.id()) || r.actor() == target || !removal.by.contains(r.actor()) {
                continue;
            }
            if let Body::Remove { member, cutoff } = r.body()
                && member == target
            {
                by_cutoff.entry(*cutoff).or_default().insert(*r.actor());
            }
        }
        if let Some((cutoff, _)) = by_cutoff
            .into_iter()
            .max_by_key(|(cutoff, signers)| (signers.len(), *cutoff))
        {
            let already_signed = log.records().skip(1).any(|r| {
                !voided.contains(r.id())
                    && r.actor() == actor
                    && matches!(
                        r.body(),
                        Body::Remove {
                            member,
                            cutoff: c,
                        } if member == target && *c == cutoff
                    )
            });
            if !already_signed {
                out.push((*target, cutoff));
            }
        }
    }
    out
}

/// Take the Mac whose age recipient is `recipient` out of this Mac's membership log as an admin's
/// revoke takes it out of the envelope: a [`Body::Remove`] whose cutoff is the last of its records
/// this log holds, else the record that let it in, saved to `paths.records_file()`, so the publish
/// that follows stamps it and sends it with its own records. Once it is in, every record that Mac
/// signs outside the cutoff's past stops counting on every Mac, and it unwraps nothing: it can no
/// longer write an entry under an epoch it still holds. `false` where this Mac has no seat in a
/// seeded log of this lineage, or the log holds no other member with that recipient.
///
/// # Errors
/// The records file cannot be read or written, or the record cannot be signed.
pub(super) fn remove_from_log(
    paths: &Paths,
    config: &Config,
    envelope_signer: &[u8; 32],
    handoff: &[([u8; 32], [u8; 64])],
    recipient: &Recipient,
) -> crate::Result<bool> {
    let (_, _, removed) = author_and_commit_records(paths, |store| {
        let Some(log) = store.log() else {
            return Ok((Vec::new(), false));
        };
        if !genesis_matches_lineage(log, config, envelope_signer, handoff) || !log.has_epochs() {
            return Ok((Vec::new(), false));
        }
        let Some((actor, device, incarnation)) = open_local_member_signer(paths, log)? else {
            return Ok((Vec::new(), false));
        };
        let targets = pending_remove_cutoffs(log, &actor, |r| r == recipient);
        if targets.is_empty() {
            return Ok((Vec::new(), false));
        }
        let authored_ms = aterm_time::unix_millis().unwrap_or(0);
        let mut log = log.clone();
        let mut minted = Vec::with_capacity(targets.len());
        device.with_seed(|seed| -> crate::Result<()> {
            for (member, cutoff) in targets {
                let record = log
                    .draft(&actor, authored_ms, Body::Remove { member, cutoff })
                    .sign(seed, incarnation)?;
                let _ = log.insert(record.clone())?;
                minted.push(record);
            }
            Ok(())
        })?;
        Ok((minted, true))
    })?;
    Ok(removed)
}

/// Records a writer drafted for a document and has not kept yet, and the log's heads once they are
/// in: what the envelope sealed with them stamps ([`stamp_line`]).
pub(super) struct Drafted {
    pub(super) records: Vec<Record>,
    pub(super) heads: Vec<RecordId>,
}

impl Drafted {
    /// The stamp the envelope sealed with these records carries.
    pub(super) fn stamp(&self) -> Option<String> {
        stamp_line(&self.heads)
    }
}

/// [`draft_document_diff`], with the drafted records added to `paths.records_file()`.
pub(super) fn append_document_diff_to_log(
    paths: &Paths,
    config: &Config,
    envelope_signer: &[u8; 32],
    handoff: &[([u8; 32], [u8; 64])],
    identity: &Identity,
    doc: &Document,
    base: Base<'_>,
) -> crate::Result<Option<Drafted>> {
    let drafted =
        draft_document_diff(paths, config, envelope_signer, handoff, identity, doc, base)?;
    if let Some(drafted) = &drafted
        && !drafted.records.is_empty()
    {
        super::save_records(paths, &encode_records(&drafted.records))?;
    }
    Ok(drafted)
}

/// Where a publish holds the records it drafted, from before its envelope goes out until a server's
/// answer says what became of it. Not the records store: an edit every server refused must never
/// enter the log, where the next overlay or mirror would publish it after all.
fn held_draft_file(paths: &Paths) -> PathBuf {
    paths.dir.join("records.drafted")
}

/// Hold `drafted` until the publish learns whether a server stored the envelope stamped with it:
/// a publish cut short after its PUT (the process ended, every reply was lost) leaves an envelope
/// whose stamp names records only this file has, which [`adopt_held_draft`] then takes in.
///
/// # Errors
/// The file cannot be written.
pub(super) fn hold_drafted(paths: &Paths, drafted: &Drafted) -> crate::Result<()> {
    fsx::write_atomic(
        &held_draft_file(paths),
        &encode_records(&drafted.records),
        0o600,
    )
}

/// Forget the held draft: kept in the records store, or refused by every server.
pub(super) fn forget_held_draft(paths: &Paths) {
    let _ = std::fs::remove_file(held_draft_file(paths));
}

/// Take in a held draft ([`hold_drafted`]) whose records `envelope`'s stamp names and this Mac's
/// log lacks: a server stored that envelope, so the edit landed, but the publish never kept its
/// records, and no other Mac has them. Without this every Mac whose log reads the version, its
/// writer included, waits on them for good. The caller holds the state lock. Returns whether the
/// records store took them.
///
/// # Errors
/// The records store or the held draft cannot be read, or the store cannot be written.
pub(super) fn adopt_held_draft(
    paths: &Paths,
    envelope: &Envelope,
    identity: &Identity,
) -> crate::Result<bool> {
    let Some(held) = fsx::read_private(
        &held_draft_file(paths),
        crate::membership_log::MAX_RECORDS_STREAM,
    )?
    else {
        return Ok(false);
    };
    let Stamp::Heads(stamp) = envelope_stamp(envelope, identity)? else {
        return Ok(false);
    };
    let mut store = super::load_records(paths)?;
    if stamp.iter().all(|id| store.contains(id)) {
        return Ok(false);
    }
    if store.ingest(held.as_slice()).is_err() || !stamp.iter().all(|id| store.contains(id)) {
        return Ok(false);
    }
    fsx::write_atomic(&paths.records_file(), &store.encode(), 0o600)?;
    forget_held_draft(paths);
    Ok(true)
}

/// Keep what [`draft_document_diff`] drafted once a server holds the envelope stamped with it:
/// add the records to `paths.records_file()`, then send the whole local store to `urls`.
///
/// # Errors
/// The records file cannot be written.
pub(super) fn keep_drafted(paths: &Paths, drafted: &Drafted, urls: &[String]) -> crate::Result<()> {
    if !drafted.records.is_empty() {
        super::save_records(paths, &encode_records(&drafted.records))?;
    }
    if urls.is_empty() {
        return Ok(());
    }
    let store = super::load_records(paths)?;
    if store.is_empty() {
        return Ok(());
    }
    let wire = store.encode();
    for url in urls {
        let _ = transport::post_records(url, &wire);
    }
    Ok(())
}

/// The recipients of each locker `doc` seals, by audience, less any `log` removed.
fn plan_recipients_of(
    doc: &Document,
    log: &Log,
) -> crate::Result<BTreeMap<Audience, BTreeSet<Recipient>>> {
    let mut out = BTreeMap::new();
    for plan in doc.lockers() {
        let mut recipients = BTreeSet::new();
        for text in &plan.recipients {
            let recipient = Recipient::parse(text).map_err(|_| Error::new("bad machine key"))?;
            if !log.recipient_is_removed(&recipient) {
                recipients.insert(recipient);
            }
        }
        out.insert(plan.audience, recipients);
    }
    Ok(out)
}

/// `doc`'s entries, by audience and name; without the Commons control entry where the log carries
/// it under its own audience (`commons_in_log`).
fn entries_by_audience(
    doc: &Document,
    commons_in_log: bool,
) -> BTreeMap<Audience, BTreeMap<String, Entry>> {
    let mut out: BTreeMap<Audience, BTreeMap<String, Entry>> = BTreeMap::new();
    for entry in doc.entries() {
        if commons_in_log && entry.name == crate::commons::ENTRY {
            continue;
        }
        out.entry(entry.audience.clone())
            .or_default()
            .insert(entry.name.clone(), entry);
    }
    out
}

/// What a writer that holds no key of `audience` drafts there, against `base`, the text the edit
/// started from: a fresh epoch wrapped to `recipients`, a [`Body::Put`] of each of `puts` and a
/// [`Body::Delete`] of each of `dropped` under it.
#[expect(
    clippy::too_many_arguments,
    reason = "one epoch's records: the writer, its seed, the audience and what goes under it"
)]
fn draft_sealed_away(
    log: &mut Log,
    actor: &MemberId,
    writer_seed: &[u8; 32],
    incarnation: [u8; 16],
    authored_ms: u64,
    audience: &Audience,
    generation: u64,
    recipients: &BTreeSet<Recipient>,
    puts: &[&Entry],
    dropped: &[&String],
    minted: &mut Vec<Record>,
) -> crate::Result<()> {
    let (draft_ep, epoch_key) = log.draft_epoch(
        actor,
        authored_ms,
        EpochAudience::from(audience),
        generation,
    )?;
    let ep_rec = draft_ep.sign(writer_seed, incarnation)?;
    let epoch_id = *ep_rec.id();
    let _ = log.insert(ep_rec.clone())?;
    minted.push(ep_rec);
    for &recipient in recipients {
        let rec = log
            .draft_wrap(actor, authored_ms, epoch_id, &epoch_key, recipient)?
            .sign(writer_seed, incarnation)?;
        let _ = log.insert(rec.clone())?;
        minted.push(rec);
    }
    for entry in puts {
        let rec = log
            .draft_put(actor, authored_ms, epoch_id, &epoch_key, entry)?
            .sign(writer_seed, incarnation)?;
        let _ = log.insert(rec.clone())?;
        minted.push(rec);
    }
    for name in dropped {
        let rec = log
            .draft_delete(actor, authored_ms, epoch_id, &epoch_key, name)?
            .sign(writer_seed, incarnation)?;
        let _ = log.insert(rec.clone())?;
        minted.push(rec);
    }
    Ok(())
}

/// What an edit started from, for [`draft_document_diff`].
#[derive(Clone, Copy, Default)]
pub(super) struct Base<'a> {
    /// The text the edit started from, where the log holds it (the version an admin opened, with
    /// the log overlaid); `None` for a version imported from its text.
    pub(super) doc: Option<&'a Document>,
    /// Names whose conflict this write settles: a person's save (`aterm secrets set`), or in a
    /// mirror an entry the merge of heads settles by its own rule. One the log holds concurrent
    /// values for is written again even where its value is the one every Mac already writes. No
    /// other write settles a conflict: the losing value stays a candidate.
    pub(super) resolve: Option<&'a BTreeSet<(Audience, String)>>,
}

/// Diff `doc` against `paths`'s seeded [`Log`] and draft the [`Body::Epoch`], [`Body::Wrap`],
/// [`Body::Put`] and [`Body::Delete`] records that write it, without keeping them. `None` where
/// this Mac cannot write the log: no seeded log of this lineage, no seat in it, or removed.
///
/// An audience whose epochs this Mac holds none of (a host list it is not in: epochs are wrapped
/// to the list's machines alone) is written against `base.doc` instead of the log it cannot read:
/// nothing where its section and machines are as `base.doc` had them, else a fresh epoch sealing
/// only the entries the edit changed and a [`Body::Delete`] of each it dropped. With no
/// `base.doc` (a version imported from its text) every entry there is put.
///
/// A name whose register holds concurrent values is left as it is unless `base.resolve` names it
/// (a person's save, or a mirror's entry the merge of heads rules on): no other write settles a
/// conflict.
pub(super) fn draft_document_diff(
    paths: &Paths,
    config: &Config,
    envelope_signer: &[u8; 32],
    handoff: &[([u8; 32], [u8; 64])],
    identity: &Identity,
    doc: &Document,
    base: Base<'_>,
) -> crate::Result<Option<Drafted>> {
    let Some((store, _)) = trusted_seeded_store(paths, config, envelope_signer, handoff)? else {
        return Ok(None);
    };
    let Some(mut log) = store.log().cloned() else {
        return Ok(None);
    };
    let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
        return Ok(None);
    };
    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut minted = Vec::new();
    let code_only: BTreeSet<String> = doc.code_only().into_iter().collect();
    if !code_only.is_empty() {
        let to_remove =
            pending_remove_cutoffs(&log, &actor, |r| code_only.contains(&r.to_bech32()));
        if !to_remove.is_empty() {
            device.with_seed(|writer_seed| -> crate::Result<()> {
                for (member, cutoff) in to_remove {
                    let rem_rec = log
                        .draft(&actor, authored_ms, Body::Remove { member, cutoff })
                        .sign(writer_seed, incarnation)?;
                    let _ = log.insert(rem_rec.clone())?;
                    minted.push(rem_rec);
                }
                Ok(())
            })?;
        }
    }
    let Some(opened) = log.open_audience_entries(identity)? else {
        return Ok(None);
    };

    let has_commons_aud = log.has_commons_epochs();
    let doc_commons_entry = doc
        .entries()
        .into_iter()
        .find(|e| e.name == crate::commons::ENTRY);
    let entries_by_aud = entries_by_audience(doc, has_commons_aud);

    let plans = doc.lockers();
    let active_plan_audiences: BTreeSet<Audience> =
        plans.iter().map(|p| p.audience.clone()).collect();
    let membership = log.membership();
    let doc_machine_recipients: BTreeSet<Recipient> = doc
        .machines()
        .into_iter()
        .filter_map(|m| Recipient::parse(&m.recipient).ok())
        .collect();
    let base_machine_recipients: BTreeSet<Recipient> = base
        .doc
        .into_iter()
        .flat_map(Document::machines)
        .filter_map(|m| Recipient::parse(&m.recipient).ok())
        .collect();
    let log_recipients = log_member_recipients(&membership, doc);
    let everyone_log_recipients: BTreeSet<Recipient> = membership
        .members()
        .iter()
        .filter(|(m_actor, _)| !log.left_out(&opened, &EpochAudience::Everyone, m_actor))
        .map(|(_, m)| m.binding.recipient())
        .filter(|r| {
            !code_only.contains(&r.to_bech32())
                && !doc_machine_recipients.contains(r)
                && !base_machine_recipients.contains(r)
        })
        .collect();
    // Audiences with epochs this Mac holds no key of, written against `base` where there is one.
    let sealed_away = |aud: &Audience| {
        let ep_aud = EpochAudience::from(aud);
        base.doc.is_some()
            && opened.maximal_epochs.contains_key(&ep_aud)
            && !opened.fully_unwrapped_audiences.contains(&ep_aud)
    };
    let mut base_recipients = base
        .doc
        .map(|base_doc| plan_recipients_of(base_doc, &log))
        .transpose()?
        .unwrap_or_default();
    // The same union as each plan's below (F15), so the log's members alone never read as a
    // change of `[everyone]`'s list.
    if let Some(everyone) = base_recipients.get_mut(&Audience::Everyone) {
        everyone.extend(everyone_log_recipients.iter().copied());
    }
    let base_entries = base
        .doc
        .map(|base_doc| entries_by_audience(base_doc, has_commons_aud))
        .unwrap_or_default();
    // A conflicted name a person saved: a `Delete` before its `Put` replaces every sibling, as a
    // `Put` of the value already winning would not (it carries that value, and leaves the others).
    let resolving = |aud: &Audience, name: &String| {
        let key = (aud.clone(), name.clone());
        opened.conflicts.contains(&key) && base.resolve.is_some_and(|names| names.contains(&key))
    };
    let next_generation = |aud: &Audience| {
        opened
            .max_generation
            .get(&EpochAudience::from(aud))
            .copied()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1)
    };
    let no_entries = BTreeMap::new();

    device.with_seed(|writer_seed| -> crate::Result<()> {
        for plan in &plans {
            let mut plan_recipients = BTreeSet::new();
            for text in &plan.recipients {
                let recipient =
                    Recipient::parse(text).map_err(|_| Error::new("bad machine key"))?;
                if !log.recipient_is_removed(&recipient) {
                    plan_recipients.insert(recipient);
                }
            }
            if plan_recipients.is_empty() {
                continue;
            }
            // `[everyone]` is the vault audience an admission wraps (§3.3.6; F18's Commons block
            // below counts the log's members for `commons`): its members are `[machines]`' and
            // the log's together, so an admin's publish keeps the epochs a member wrapped to a
            // Mac it admitted instead of minting one that leaves that Mac out.
            if plan.audience == Audience::Everyone {
                plan_recipients.extend(everyone_log_recipients.iter().copied());
            }
            let doc_map = entries_by_aud.get(&plan.audience);
            if sealed_away(&plan.audience) {
                let before = base_entries.get(&plan.audience).unwrap_or(&no_entries);
                let now = doc_map.unwrap_or(&no_entries);
                // New or departed machines: every entry is sealed again for the list as it is.
                let resealed = base_recipients.get(&plan.audience) != Some(&plan_recipients);
                let puts: Vec<&Entry> = now
                    .values()
                    .filter(|e| resealed || before.get(&e.name).is_none_or(|b| !same_entry(b, e)))
                    .collect();
                let dropped: Vec<&String> =
                    before.keys().filter(|n| !now.contains_key(*n)).collect();
                if resealed || !puts.is_empty() || !dropped.is_empty() {
                    draft_sealed_away(
                        &mut log,
                        &actor,
                        writer_seed,
                        incarnation,
                        authored_ms,
                        &plan.audience,
                        next_generation(&plan.audience),
                        &plan_recipients,
                        &puts,
                        &dropped,
                        &mut minted,
                    )?;
                }
                continue;
            }
            let ep_aud = EpochAudience::from(&plan.audience);
            let log_map = opened.by_audience.get(&plan.audience);
            if matches!(plan.audience, Audience::Hosts(_))
                && doc_map.is_none_or(BTreeMap::is_empty)
                && log_map.is_none_or(BTreeMap::is_empty)
            {
                continue;
            }
            let max_ids = opened
                .maximal_epochs
                .get(&ep_aud)
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            let mut active_epochs: BTreeSet<RecordId> = max_ids.iter().copied().collect();
            if let Some(winning) = opened.winning_epochs.get(&ep_aud) {
                active_epochs.extend(winning.iter().copied());
            }
            // Whoever minted an epoch holds its key whether or not any Wrap names it, so an epoch
            // minted by a Mac this plan does not reach (any member can sign an Epoch, and a Mac
            // retired from `[machines]` stays a log member until F16) counts as departed, as one
            // wrapped to such a Mac does: reusing it would seal new writes to that Mac.
            let minted_outside = active_epochs.iter().any(|ep_id| {
                let Some(minter) = log.get(ep_id).map(|record| *record.actor()) else {
                    return true;
                };
                minter != actor
                    && membership
                        .get(&minter)
                        .is_none_or(|m| !plan_recipients.contains(&m.binding.recipient()))
            });
            let departed_wrapped = minted_outside
                || active_epochs.iter().any(|ep_id| {
                    opened
                        .wrapped_recipients
                        .get(ep_id)
                        .is_some_and(|wrapped| wrapped.iter().any(|r| !plan_recipients.contains(r)))
                });
            // An entry that leaves `[everyone]` (a `Delete`, or narrowed to a host list) leaves
            // its old value sealed under the epoch it was put at. Any member wraps `[everyone]`'s
            // newest and winning epochs to a Mac it admits (F15) and hands it every record, so
            // reusing that epoch would let every later Mac read the value removed before it came:
            // mint a fresh one and reseal what stays, so the old epoch is neither newest nor
            // winning.
            let left_everyone = plan.audience == Audience::Everyone
                && log_map.is_some_and(|log_entries| {
                    log_entries
                        .keys()
                        .any(|name| doc_map.is_none_or(|m| !m.contains_key(name)))
                });
            let reusable_epoch: Option<([u8; 32], [u8; 32])> =
                if !departed_wrapped && !left_everyone && max_ids.len() == 1 {
                    opened
                        .unwrapped_keys
                        .get(&max_ids[0])
                        .map(|key| (max_ids[0], **key))
                } else {
                    None
                };

            if let Some((epoch_id, epoch_key)) = reusable_epoch {
                // Close any Wrap gaps across all active epochs of this audience that we hold.
                for ep_id in &active_epochs {
                    let Some(ep_key) = opened.unwrapped_keys.get(ep_id) else {
                        continue;
                    };
                    let already = opened.wrapped_recipients.get(ep_id);
                    for &recipient in &plan_recipients {
                        if already.is_none_or(|set| !set.contains(&recipient)) {
                            let wrap_rec = log
                                .draft_wrap(&actor, authored_ms, *ep_id, ep_key, recipient)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(wrap_rec.clone())?;
                            minted.push(wrap_rec);
                        }
                    }
                }

                if let Some(doc_entries) = doc_map {
                    for (name, entry) in doc_entries {
                        let resolve = resolving(&plan.audience, name);
                        let unchanged = !resolve
                            && log_map
                                .and_then(|m| m.get(name))
                                .is_some_and(|cur| same_entry(cur, entry));
                        if resolve {
                            let del_rec = log
                                .draft_delete(&actor, authored_ms, epoch_id, &epoch_key, name)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(del_rec.clone())?;
                            minted.push(del_rec);
                        }
                        if !unchanged {
                            let put_rec = log
                                .draft_put(&actor, authored_ms, epoch_id, &epoch_key, entry)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(put_rec.clone())?;
                            minted.push(put_rec);
                        }
                    }
                }
                if let Some(log_entries) = log_map {
                    for name in log_entries.keys() {
                        if doc_map.is_none_or(|m| !m.contains_key(name)) {
                            let del_rec = log
                                .draft_delete(&actor, authored_ms, epoch_id, &epoch_key, name)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(del_rec.clone())?;
                            minted.push(del_rec);
                        }
                    }
                }
            } else {
                let next_gen = opened
                    .max_generation
                    .get(&ep_aud)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .max(1);
                let (draft_ep, epoch_key) =
                    log.draft_epoch(&actor, authored_ms, ep_aud, next_gen)?;
                let ep_rec = draft_ep.sign(writer_seed, incarnation)?;
                let epoch_id = *ep_rec.id();
                let _ = log.insert(ep_rec.clone())?;
                minted.push(ep_rec);

                for recipient in plan_recipients {
                    let wrap_rec = log
                        .draft_wrap(&actor, authored_ms, epoch_id, &epoch_key, recipient)?
                        .sign(writer_seed, incarnation)?;
                    let _ = log.insert(wrap_rec.clone())?;
                    minted.push(wrap_rec);
                }
                if let Some(doc_entries) = doc_map {
                    for (name, entry) in doc_entries {
                        if resolving(&plan.audience, name) {
                            let del_rec = log
                                .draft_delete(&actor, authored_ms, epoch_id, &epoch_key, name)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(del_rec.clone())?;
                            minted.push(del_rec);
                        }
                        let put_rec = log
                            .draft_put(&actor, authored_ms, epoch_id, &epoch_key, entry)?
                            .sign(writer_seed, incarnation)?;
                        let _ = log.insert(put_rec.clone())?;
                        minted.push(put_rec);
                    }
                }
                if let Some(log_entries) = log_map {
                    for name in log_entries.keys() {
                        if doc_map.is_none_or(|m| !m.contains_key(name)) {
                            let del_rec = log
                                .draft_delete(&actor, authored_ms, epoch_id, &epoch_key, name)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(del_rec.clone())?;
                            minted.push(del_rec);
                        }
                    }
                }
            }
        }

        // Maintain `EpochAudience::Commons` wraps and control descriptor when `doc` carries one.
        if has_commons_aud && let Some(ctrl_entry) = &doc_commons_entry {
            let mut commons_recipients: BTreeSet<Recipient> = membership
                .members()
                .iter()
                .filter(|(m_actor, _)| {
                    !log.left_out(&opened, &EpochAudience::Commons, m_actor)
                        && !log.left_out(&opened, &EpochAudience::Everyone, m_actor)
                })
                .map(|(_, m)| m.binding.recipient())
                .filter(|r| {
                    !code_only.contains(&r.to_bech32())
                        && !doc_machine_recipients.contains(r)
                        && !base_machine_recipients.contains(r)
                })
                .collect();
            if let Audience::Hosts(hosts) = &ctrl_entry.audience {
                let by_name: BTreeMap<String, Recipient> = doc
                    .machines()
                    .into_iter()
                    .filter_map(|m| {
                        (!code_only.contains(&m.recipient))
                            .then(|| Recipient::parse(&m.recipient).ok().map(|r| (m.name, r)))
                            .flatten()
                    })
                    .collect();
                for host in hosts {
                    if let Some(&r) = by_name.get(host)
                        && !log.recipient_is_removed(&r)
                    {
                        commons_recipients.insert(r);
                    }
                }
            }
            if !commons_recipients.is_empty() {
                let max_ids = opened
                    .maximal_epochs
                    .get(&EpochAudience::Commons)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let mut active_epochs: BTreeSet<RecordId> = max_ids.iter().copied().collect();
                if let Some(winning) = opened.winning_epochs.get(&EpochAudience::Commons) {
                    active_epochs.extend(winning.iter().copied());
                }
                let minted_outside = active_epochs.iter().any(|ep_id| {
                    let Some(minter) = log.get(ep_id).map(|record| *record.actor()) else {
                        return true;
                    };
                    minter != actor
                        && membership
                            .get(&minter)
                            .is_none_or(|m| !commons_recipients.contains(&m.binding.recipient()))
                });
                let departed_wrapped = minted_outside
                    || active_epochs.iter().any(|ep_id| {
                        opened.wrapped_recipients.get(ep_id).is_some_and(|wrapped| {
                            wrapped.iter().any(|r| !commons_recipients.contains(r))
                        })
                    });
                let reusable_commons = if !departed_wrapped && max_ids.len() == 1 {
                    opened
                        .unwrapped_keys
                        .get(&max_ids[0])
                        .map(|key| (max_ids[0], **key))
                } else {
                    None
                };
                if let Some((ep_id, ep_key)) = reusable_commons {
                    for active_id in &active_epochs {
                        let Some(active_key) = opened.unwrapped_keys.get(active_id) else {
                            continue;
                        };
                        let already = opened.wrapped_recipients.get(active_id);
                        for &recipient in &commons_recipients {
                            if already.is_none_or(|set| !set.contains(&recipient)) {
                                let wrap_rec = log
                                    .draft_wrap(
                                        &actor,
                                        authored_ms,
                                        *active_id,
                                        active_key,
                                        recipient,
                                    )?
                                    .sign(writer_seed, incarnation)?;
                                let _ = log.insert(wrap_rec.clone())?;
                                minted.push(wrap_rec);
                            }
                        }
                    }
                    if opened.commons_control.as_ref().is_none_or(|cur| {
                        !aterm_digest::ct_eq(cur.as_slice(), ctrl_entry.value.as_slice())
                    }) {
                        let put_rec = log
                            .draft_commons_put(
                                &actor,
                                authored_ms,
                                ep_id,
                                &ep_key,
                                ctrl_entry.value.as_slice(),
                            )?
                            .sign(writer_seed, incarnation)?;
                        let _ = log.insert(put_rec.clone())?;
                        minted.push(put_rec);
                    }
                } else {
                    let next_gen = opened
                        .max_generation
                        .get(&EpochAudience::Commons)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(1)
                        .max(1);
                    let (draft_ep, ep_key) =
                        log.draft_epoch(&actor, authored_ms, EpochAudience::Commons, next_gen)?;
                    let ep_rec = draft_ep.sign(writer_seed, incarnation)?;
                    let ep_id = *ep_rec.id();
                    let _ = log.insert(ep_rec.clone())?;
                    minted.push(ep_rec);
                    for &recipient in &commons_recipients {
                        let wrap_rec = log
                            .draft_wrap(&actor, authored_ms, ep_id, &ep_key, recipient)?
                            .sign(writer_seed, incarnation)?;
                        let _ = log.insert(wrap_rec.clone())?;
                        minted.push(wrap_rec);
                    }
                    let put_rec = log
                        .draft_commons_put(
                            &actor,
                            authored_ms,
                            ep_id,
                            &ep_key,
                            ctrl_entry.value.as_slice(),
                        )?
                        .sign(writer_seed, incarnation)?;
                    let _ = log.insert(put_rec.clone())?;
                    minted.push(put_rec);
                }
                // Close any missing wraps on historical `EpochAudience::Commons` epochs for active
                // bound members so they can decrypt Commons table rows sealed under earlier epochs.
                let historical_commons: Vec<RecordId> = log
                    .records()
                    .skip(1)
                    .filter_map(|r| match r.body() {
                        Body::Epoch {
                            audience: EpochAudience::Commons,
                            ..
                        } if !active_epochs.contains(r.id())
                            && opened.unwrapped_keys.contains_key(r.id()) =>
                        {
                            Some(*r.id())
                        }
                        _ => None,
                    })
                    .collect();
                for hist_id in historical_commons {
                    let Some(hist_key) = opened.unwrapped_keys.get(&hist_id) else {
                        continue;
                    };
                    let already = opened.wrapped_recipients.get(&hist_id);
                    for &recipient in commons_recipients.intersection(&log_recipients) {
                        if already.is_none_or(|set| !set.contains(&recipient)) {
                            let wrap_rec = log
                                .draft_wrap(&actor, authored_ms, hist_id, hist_key, recipient)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(wrap_rec.clone())?;
                            minted.push(wrap_rec);
                        }
                    }
                }
            }
        }

        // Tombstone entries in any unwrapped host audience section removed completely from `doc`.
        for (aud, log_entries) in &opened.by_audience {
            if active_plan_audiences.contains(aud) || log_entries.is_empty() {
                continue;
            }
            let ep_aud = EpochAudience::from(aud);
            let Some(max_ids) = opened.maximal_epochs.get(&ep_aud) else {
                continue;
            };
            let Some((&ep_id, ep_key)) = max_ids
                .iter()
                .find_map(|id| opened.unwrapped_keys.get(id).map(|k| (id, k)))
            else {
                continue;
            };
            for name in log_entries.keys() {
                let del_rec = log
                    .draft_delete(&actor, authored_ms, ep_id, ep_key, name)?
                    .sign(writer_seed, incarnation)?;
                let _ = log.insert(del_rec.clone())?;
                minted.push(del_rec);
            }
        }

        // The same for a section this Mac holds no key of that `doc` dropped whole: a fresh epoch
        // for the list's machines `doc` still names, and a `Delete` of each name `base` held.
        let doc_recipients: BTreeSet<Recipient> = doc
            .machines()
            .iter()
            .filter_map(|m| Recipient::parse(&m.recipient).ok())
            .filter(|r| !log.recipient_is_removed(r))
            .collect();
        for (aud, before) in &base_entries {
            if active_plan_audiences.contains(aud) || before.is_empty() || !sealed_away(aud) {
                continue;
            }
            let still: BTreeSet<Recipient> = base_recipients
                .get(aud)
                .map(|r| r.intersection(&doc_recipients).copied().collect())
                .unwrap_or_default();
            if still.is_empty() {
                continue;
            }
            let dropped: Vec<&String> = before.keys().collect();
            draft_sealed_away(
                &mut log,
                &actor,
                writer_seed,
                incarnation,
                authored_ms,
                aud,
                next_generation(aud),
                &still,
                &[],
                &dropped,
                &mut minted,
            )?;
        }
        Ok(())
    })?;

    Ok(Some(Drafted {
        records: minted,
        heads: log.heads(),
    }))
}

/// Load the locally held [`Log`] and its heads digest when it carries [`EpochAudience::Commons`]
/// epochs (`F18`), matches `trust`'s `(network, authority)`, and aligns with the configured vault
/// lineage.
pub fn trusted_commons_log(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
) -> crate::Result<Option<(Log, [u8; 32])>> {
    let store = super::load_records(paths)?;
    trusted_commons_log_from_store(paths, &store, trust)
}

fn trusted_commons_log_from_store(
    paths: &Paths,
    store: &Store,
    trust: crate::commons::journal::Trust,
) -> crate::Result<Option<(Log, [u8; 32])>> {
    let Some(log) = store.log() else {
        return Ok(None);
    };
    if !log.has_commons_epochs() {
        return Ok(None);
    }
    let Body::Genesis { root, .. } = log.genesis().body() else {
        return Ok(None);
    };
    if *root
        != (Root {
            network: trust.network,
            authority: trust.authority,
        })
    {
        return Ok(None);
    }
    if !commons_log_matches_local_lineage(paths, log)? {
        return Ok(None);
    }
    let digest = records_heads_digest(log);
    Ok(Some((log.clone(), digest)))
}

struct RecordsAuthorLock(std::fs::File);

impl RecordsAuthorLock {
    fn take(paths: &Paths) -> crate::Result<Self> {
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.dir.join(".records.author.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of RecordsAuthorLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        file.lock()
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        Ok(Self(file))
    }

    fn try_take(paths: &Paths) -> crate::Result<Option<Self>> {
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.dir.join(".records.author.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the non-blocking take of RecordsAuthorLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(Error::new(format!("{}: {e}", path.display())))
            }
        }
    }

    fn try_take_until(paths: &Paths, deadline: Instant) -> crate::Result<Option<Self>> {
        let mut backoff = Duration::from_millis(1);
        loop {
            if let Some(lock) = Self::try_take(paths)? {
                return Ok(Some(lock));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            std::thread::sleep(backoff.min(remaining));
            backoff = (backoff * 2).min(Duration::from_millis(50));
        }
    }
}

impl Drop for RecordsAuthorLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Serialize local membership-log authoring under [`RecordsAuthorLock`] and commit `minted`
/// records under [`StateLock`], re-drafting up to 3 times if concurrent peer ingress extended
/// `paths.records_file()` while signing.
fn author_and_commit_records<T>(
    paths: &Paths,
    mut draft: impl FnMut(&Store) -> crate::Result<(Vec<Record>, T)>,
) -> crate::Result<(Vec<Record>, Store, T)> {
    let _author = RecordsAuthorLock::take(paths)?;
    for attempt in 0..4 {
        let snapshot = super::load_records(paths)?;
        let before = snapshot.inventory();
        let (minted, out) = draft(&snapshot)?;
        if minted.is_empty() {
            return Ok((minted, snapshot, out));
        }
        let wire = encode_records(&minted);
        let _state = StateLock::take(paths)?;
        let mut current = super::load_records(paths)?;
        if attempt < 3 && current.inventory() != before {
            continue;
        }
        let added = current.ingest(&wire)?;
        if added > 0 {
            fsx::write_atomic(&paths.records_file(), &current.encode(), 0o600)?;
        }
        return Ok((minted, current, out));
    }
    unreachable!("bounded loop returns on attempt == 3")
}

/// Bounded variant of [`author_and_commit_records`] that waits at most until `deadline` for
/// [`RecordsAuthorLock`] and [`StateLock`], returning `Ok(None)` when another local writer holds
/// either lock past `deadline`.
fn author_and_commit_records_until<T>(
    paths: &Paths,
    deadline: Instant,
    mut draft: impl FnMut(&Store) -> crate::Result<(Vec<Record>, T)>,
) -> crate::Result<Option<(Vec<Record>, Store, T)>> {
    let Some(_author) = RecordsAuthorLock::try_take_until(paths, deadline)? else {
        return Ok(None);
    };
    for attempt in 0..4 {
        let snapshot = super::load_records(paths)?;
        let before = snapshot.inventory();
        let (minted, out) = draft(&snapshot)?;
        if minted.is_empty() {
            return Ok(Some((minted, snapshot, out)));
        }
        let wire = encode_records(&minted);
        let Some(_state) = StateLock::try_take_until(paths, deadline)? else {
            return Ok(None);
        };
        let mut current = super::load_records(paths)?;
        if attempt < 3 && current.inventory() != before {
            continue;
        }
        let added = current.ingest(&wire)?;
        if added > 0 {
            fsx::write_atomic(&paths.records_file(), &current.encode(), 0o600)?;
        }
        return Ok(Some((minted, current, out)));
    }
    unreachable!("bounded loop returns on attempt == 3")
}

pub(super) fn commons_log_matches_local_lineage(paths: &Paths, log: &Log) -> crate::Result<bool> {
    let Body::Genesis { signer, .. } = log.genesis().body() else {
        return Ok(false);
    };
    genesis_signer_matches_local_lineage(paths, signer)
}

pub(super) fn genesis_signer_matches_local_lineage(
    paths: &Paths,
    signer: &[u8; 32],
) -> crate::Result<bool> {
    if let Some(cfg) = super::config(paths)? {
        let mut lineage_ok = cfg.signer.as_ref() == Some(signer) || cfg.followed.contains(signer);
        if !lineage_ok
            && let Ok(Some(bytes)) = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
            && let Ok(env) = envelope::parse(bytes.as_slice())
        {
            lineage_ok = signer == &env.signer
                || cfg.signer.as_ref() == Some(signer)
                || cfg.followed.contains(signer)
                || env.handoff.iter().any(|(key, _)| key == signer);
        }
        if cfg.signer.is_some() && !lineage_ok {
            return Ok(false);
        }
    }
    Ok(true)
}

fn post_records_best_effort(paths: &Paths, wire: &[u8]) {
    if let Ok(Some(cfg)) = super::config(paths)
        && let Source::Url(url) = &cfg.source
    {
        let _ = transport::post_records(url, wire);
    }
}

struct AuthorCtx<'a> {
    actor: &'a MemberId,
    writer_seed: &'a [u8; 32],
    incarnation: [u8; 16],
    authored_ms: u64,
}

fn wrap_unwrapped_commons_and_everyone(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    author: &AuthorCtx<'_>,
    commons_recipients: &BTreeSet<Recipient>,
    everyone_recipients: &BTreeSet<Recipient>,
    minted: &mut Vec<Record>,
) -> crate::Result<()> {
    let mut target_epochs: Vec<(EpochAudience, RecordId)> = Vec::new();
    for rec in log.records().skip(1) {
        if let Body::Epoch { audience, .. } = rec.body() {
            let ep_id = *rec.id();
            if !opened.unwrapped_keys.contains_key(&ep_id) {
                continue;
            }
            match audience {
                EpochAudience::Commons => target_epochs.push((EpochAudience::Commons, ep_id)),
                EpochAudience::Everyone => {
                    let is_active = opened
                        .maximal_epochs
                        .get(&EpochAudience::Everyone)
                        .is_some_and(|m| m.contains(&ep_id))
                        || opened
                            .winning_epochs
                            .get(&EpochAudience::Everyone)
                            .is_some_and(|w| w.contains(&ep_id));
                    if is_active {
                        target_epochs.push((EpochAudience::Everyone, ep_id));
                    }
                }
                EpochAudience::Hosts(_) => {}
            }
        }
    }
    for (aud, ep_id) in target_epochs {
        let Some(ep_key) = opened.unwrapped_keys.get(&ep_id) else {
            continue;
        };
        let recipients = match aud {
            EpochAudience::Commons => commons_recipients,
            EpochAudience::Everyone => everyone_recipients,
            EpochAudience::Hosts(_) => continue,
        };
        let already = opened.wrapped_recipients.get(&ep_id);
        for &recipient in recipients {
            if already.is_none_or(|set| !set.contains(&recipient)) {
                let wrap_rec = log
                    .draft_wrap(author.actor, author.authored_ms, ep_id, ep_key, recipient)?
                    .sign(author.writer_seed, author.incarnation)?;
                let _ = log.insert(wrap_rec.clone())?;
                minted.push(wrap_rec);
            }
        }
    }
    Ok(())
}

/// Sign and publish [`Body::Node`] for this Mac's fleet `node_key` when this Mac is an active
/// bound member in the local membership log and its current log-bound node key differs from
/// `node_key` (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3, F17).
///
/// `sign_proof` receives `(network, actor)` and returns the Ed25519 signature by `node_key` over
/// [`crate::membership_log::node_proof_message`]. Returns `Ok(None)` when no membership log is
/// present, this Mac is not an active bound member, or `node_key` is already bound in the log.
///
/// # Errors
/// Returns an error if the proof signature does not verify or the record store cannot be
/// updated.
pub fn publish_local_node_key(
    paths: &Paths,
    node_key: [u8; 32],
    sign_proof: impl FnOnce(&RecordId, &MemberId) -> [u8; 64],
) -> crate::Result<Option<Record>> {
    if node_key == [0; 32] {
        return Ok(None);
    }
    let store = load_records(paths)?;
    let Some(log) = store.log() else {
        return Ok(None);
    };
    let Some((actor, _, _)) = open_local_member_signer(paths, log)? else {
        return Ok(None);
    };
    if log.membership().get(&actor).and_then(|m| m.node_key) == Some(node_key) {
        return Ok(None);
    }
    let mut sign_opt = Some(sign_proof);
    let (minted, _, ()) = author_and_commit_records(paths, |store| {
        let Some(mut log) = store.log().cloned() else {
            return Ok((Vec::new(), ()));
        };
        let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
            return Ok((Vec::new(), ()));
        };
        if log.membership().get(&actor).and_then(|m| m.node_key) == Some(node_key) {
            return Ok((Vec::new(), ()));
        }
        let Some(sign_fn) = sign_opt.take() else {
            return Ok((Vec::new(), ()));
        };
        let proof = sign_fn(log.network(), &actor);
        let authored_ms = aterm_time::unix_millis().unwrap_or(0);
        let mut minted = Vec::new();
        device.with_seed(|writer_seed| -> crate::Result<()> {
            let rec = log
                .draft(&actor, authored_ms, Body::Node { node_key, proof })
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(rec.clone())?;
            minted.push(rec);
            Ok(())
        })?;
        Ok((minted, ()))
    })?;
    if !minted.is_empty() {
        post_records_best_effort(paths, &encode_records(&minted));
    }
    Ok(minted.into_iter().next())
}

/// Sign [`Body::Admit`] for `entrant` with this member's Commons device key and wrap every
/// unwrapped [`EpochAudience::Commons`] and active [`EpochAudience::Everyone`] epoch to `entrant`'s
/// recipient (`F18`).
///
/// # Errors
/// Local custody holds no seeded log, this Mac is not an active bound member in the log, or the
/// admission violates log invariants.
pub fn admit_member_in_log(
    paths: &Paths,
    entrant: Entrant,
    evidence: Evidence,
) -> crate::Result<Vec<Record>> {
    let (minted, _, ()) = author_and_commit_records(paths, |store| {
        Ok((
            draft_admit_member_in_log(paths, store, entrant.clone(), evidence)?,
            (),
        ))
    })?;
    if !minted.is_empty() {
        post_records_best_effort(paths, &encode_records(&minted));
    }
    Ok(minted)
}

fn draft_admit_member_in_log(
    paths: &Paths,
    store: &Store,
    entrant: Entrant,
    evidence: Evidence,
) -> crate::Result<Vec<Record>> {
    let Some(mut log) = store.log().cloned() else {
        return Err(Error::new("this machine holds no membership log"));
    };
    if !log.has_commons_epochs() {
        return Err(Error::new(
            "this membership log has no Commons control epochs",
        ));
    }
    let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
        return Err(Error::new(
            "this machine is not an active bound member in the membership log",
        ));
    };
    let identity = super::read_identity(paths)?
        .ok_or_else(|| Error::new("this machine has no secrets identity"))?;
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Err(Error::new("this machine cannot open membership log epochs"));
    };

    let entrant_recipient = entrant.request.binding().recipient();
    let entrant_actor = entrant.member();
    let membership = log.membership();
    if membership.removal(&entrant_actor).is_some() || log.recipient_is_removed(&entrant_recipient)
    {
        return Err(Error::new(
            "the vault removed this Mac, and an Admit does not let a removed Mac back in",
        ));
    }
    if membership.members().iter().any(|(m_actor, m)| {
        (*m_actor == entrant_actor || m.binding.recipient() == entrant_recipient)
            && log.left_out(&opened, &EpochAudience::Everyone, m_actor)
    }) || log.audience_left_out_recipient(&EpochAudience::Everyone, &entrant_recipient)
    {
        return Err(Error::new(
            "the vault's newest key leaves this Mac out on purpose, and an Admit does not let it back in",
        ));
    }
    if membership.members().iter().any(|(m_actor, m)| {
        (*m_actor == entrant_actor || m.binding.recipient() == entrant_recipient)
            && &m.binding != entrant.request.binding()
    }) {
        return Err(Error::new(
            "this Mac's key is already bound to a different seat in the membership log",
        ));
    }
    let already_active = membership
        .members()
        .get(&entrant_actor)
        .is_some_and(|m| &m.binding == entrant.request.binding());
    let commons_left_out =
        already_active && log.left_out(&opened, &EpochAudience::Commons, &entrant_actor);

    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut minted = Vec::new();
    device.with_seed(|writer_seed| -> crate::Result<()> {
        if !already_active {
            let admit_rec = log
                .draft(&actor, authored_ms, Body::Admit { entrant, evidence })
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(admit_rec.clone())?;
            minted.push(admit_rec);
        }
        let recipients = BTreeSet::from([entrant_recipient]);
        let empty_recipients = BTreeSet::new();
        let commons_recipients = if commons_left_out {
            &empty_recipients
        } else {
            &recipients
        };
        wrap_unwrapped_commons_and_everyone(
            &mut log,
            &opened,
            &AuthorCtx {
                actor: &actor,
                writer_seed,
                incarnation,
                authored_ms,
            },
            commons_recipients,
            &recipients,
            &mut minted,
        )?;
        Ok(())
    })?;
    Ok(minted)
}

/// Sign [`Body::Remove`] for `target` with this member's Commons device key and rotate both
/// [`EpochAudience::Everyone`] and [`EpochAudience::Commons`] epochs to the remaining active
/// members (`F18`, `§3.3.7`).
///
/// # Errors
/// Local custody holds no seeded log, this Mac is not an active bound member, or `target` has no
/// entry in the log.
pub fn remove_member_in_log(paths: &Paths, target: MemberId) -> crate::Result<Vec<Record>> {
    let minted = remove_member_in_log_local(paths, target, true)?;
    if !minted.is_empty() {
        let wire = encode_records(&minted);
        post_records_best_effort(paths, &wire);
    }
    Ok(minted)
}

pub(super) fn remove_member_in_log_local(
    paths: &Paths,
    target: MemberId,
    take_state_lock: bool,
) -> crate::Result<Vec<Record>> {
    if take_state_lock {
        let (minted, _, ()) = author_and_commit_records(paths, |store| {
            Ok((
                draft_retire_in_log(paths, store, &[target], &BTreeSet::new())?,
                (),
            ))
        })?;
        return Ok(minted);
    }
    retire_recipients_in_log_local(paths, &[target], &BTreeSet::new())
}

pub(super) fn retire_recipients_in_log_local(
    paths: &Paths,
    active_targets: &[MemberId],
    exclude_recipients: &BTreeSet<Recipient>,
) -> crate::Result<Vec<Record>> {
    let store = super::load_records(paths)?;
    let minted = draft_retire_in_log(paths, &store, active_targets, exclude_recipients)?;
    if !minted.is_empty() {
        let wire = encode_records(&minted);
        let _ = super::save_records_unlocked(paths, &wire)?;
        aterm_durable::durable_dir(&paths.dir).map_err(|e| Error::new(e.to_string()))?;
        post_records_best_effort(paths, &wire);
    }
    Ok(minted)
}

fn draft_retire_in_log(
    paths: &Paths,
    store: &Store,
    targets: &[MemberId],
    exclude_recipients: &BTreeSet<Recipient>,
) -> crate::Result<Vec<Record>> {
    let Some(mut log) = store.log().cloned() else {
        return Err(Error::new("this machine holds no membership log"));
    };
    if !log.has_commons_epochs() {
        return Err(Error::new(
            "this membership log has no Commons control epochs",
        ));
    }
    let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
        return Err(Error::new(
            "this machine is not an active bound member in the membership log",
        ));
    };
    if targets.contains(&actor) {
        return Err(Error::new("a member does not remove itself"));
    }
    let identity = super::read_identity(paths)?
        .ok_or_else(|| Error::new("this machine has no secrets identity"))?;
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Err(Error::new("this machine cannot open membership log epochs"));
    };

    let membership = log.membership();
    let voided = membership.voided();
    let mut cutoffs = Vec::with_capacity(targets.len());
    let mut any_was_active = false;
    for &target in targets {
        let cutoff = if let Some(member) = membership.get(&target) {
            any_was_active = true;
            log.records()
                .skip(1)
                .filter(|r| r.actor() == &target)
                .max_by(|a, b| a.sequence().cmp(&b.sequence()).then(b.id().cmp(a.id())))
                .map_or(member.entry, |r| *r.id())
        } else if let Some(removal) = membership.removal(&target) {
            if let Some(q) = membership.quorums().iter().find(|q| q.member == target) {
                q.cutoff
            } else {
                let mut by_cutoff: BTreeMap<RecordId, BTreeSet<MemberId>> = BTreeMap::new();
                for r in log.records().skip(1) {
                    if voided.contains(r.id())
                        || r.actor() == &target
                        || !removal.by.contains(r.actor())
                    {
                        continue;
                    }
                    if let Body::Remove { member, cutoff } = r.body()
                        && *member == target
                    {
                        by_cutoff.entry(*cutoff).or_default().insert(*r.actor());
                    }
                }
                by_cutoff
                    .into_iter()
                    .max_by_key(|(cutoff, signers)| {
                        (!signers.contains(&actor), signers.len(), *cutoff)
                    })
                    .map(|(cutoff, _)| cutoff)
                    .ok_or_else(|| {
                        Error::new("target member is not active in the membership log")
                    })?
            }
        } else {
            return Err(Error::new(
                "target member is not active in the membership log",
            ));
        };
        let already_signed = log.records().skip(1).any(|r| {
            !voided.contains(r.id())
                && r.actor() == &actor
                && matches!(
                    r.body(),
                    Body::Remove {
                        member,
                        cutoff: c,
                    } if *member == target && *c == cutoff
                )
        });
        if !already_signed {
            cutoffs.push((target, cutoff));
        }
    }

    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut minted = Vec::new();
    device.with_seed(|writer_seed| -> crate::Result<()> {
        for (target, cutoff) in cutoffs {
            let rem_rec = log
                .draft(
                    &actor,
                    authored_ms,
                    Body::Remove {
                        member: target,
                        cutoff,
                    },
                )
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(rem_rec.clone())?;
            minted.push(rem_rec);
        }
        let post_opened = if minted.is_empty() {
            opened
        } else {
            log.open_audience_entries(&identity)?
                .ok_or_else(|| Error::new("this machine cannot open membership log epochs"))?
        };
        let removed_recs = log.removed_recipients();
        let epochs_need_rotation = any_was_active
            || !exclude_recipients.is_empty()
            || [EpochAudience::Everyone, EpochAudience::Commons]
                .iter()
                .any(|aud| {
                    post_opened.maximal_epochs.get(aud).is_some_and(|max_ids| {
                        max_ids.len() > 1
                            || max_ids.iter().any(|ep_id| {
                                post_opened
                                    .wrapped_recipients
                                    .get(ep_id)
                                    .is_some_and(|set| !set.is_disjoint(&removed_recs))
                            })
                    })
                });
        if epochs_need_rotation {
            rotate_epochs_after_remove(
                &mut log,
                &post_opened,
                &AuthorCtx {
                    actor: &actor,
                    writer_seed,
                    incarnation,
                    authored_ms,
                },
                exclude_recipients,
                &mut minted,
            )?;
        }
        Ok(())
    })?;
    Ok(minted)
}

fn carried_audience_recipients(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    audience: &EpochAudience,
    active_members: &BTreeSet<Recipient>,
    exclude_recipients: &BTreeSet<Recipient>,
) -> BTreeSet<Recipient> {
    let removed = log.removed_recipients();
    let left_out_recipients: BTreeSet<Recipient> = log
        .membership()
        .members()
        .iter()
        .filter(|(actor, _)| {
            log.left_out(opened, audience, actor)
                || log.left_out(opened, &EpochAudience::Everyone, actor)
        })
        .map(|(_, m)| m.binding.recipient())
        .collect();
    let mut out: BTreeSet<Recipient> = active_members
        .iter()
        .copied()
        .filter(|r| {
            !removed.contains(r)
                && !exclude_recipients.contains(r)
                && !left_out_recipients.contains(r)
        })
        .collect();
    if let Some(max_ids) = opened.maximal_epochs.get(audience)
        && let Some((first, rest)) = max_ids.split_first()
        && let Some(first_wrapped) = opened.wrapped_recipients.get(first)
    {
        for &r in first_wrapped {
            if !removed.contains(&r)
                && !exclude_recipients.contains(&r)
                && !left_out_recipients.contains(&r)
                && rest.iter().all(|ep_id| {
                    opened
                        .wrapped_recipients
                        .get(ep_id)
                        .is_some_and(|set| set.contains(&r))
                })
            {
                out.insert(r);
            }
        }
    }
    out
}

fn rotate_epochs_after_remove(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    author: &AuthorCtx<'_>,
    exclude_recipients: &BTreeSet<Recipient>,
    minted: &mut Vec<Record>,
) -> crate::Result<()> {
    let membership = log.membership();
    let remaining_members: BTreeSet<Recipient> = membership
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .filter(|r| !exclude_recipients.contains(r))
        .collect();
    if remaining_members.is_empty() {
        return Ok(());
    }
    let everyone_recipients = carried_audience_recipients(
        log,
        opened,
        &EpochAudience::Everyone,
        &remaining_members,
        exclude_recipients,
    );
    let commons_recipients = carried_audience_recipients(
        log,
        opened,
        &EpochAudience::Commons,
        &remaining_members,
        exclude_recipients,
    );

    if opened.max_generation.contains_key(&EpochAudience::Everyone) {
        let next_gen = opened
            .max_generation
            .get(&EpochAudience::Everyone)
            .copied()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        let (ep_draft, ep_key) = log.draft_epoch(
            author.actor,
            author.authored_ms,
            EpochAudience::Everyone,
            next_gen,
        )?;
        let ep_rec = ep_draft.sign(author.writer_seed, author.incarnation)?;
        let ep_id = *ep_rec.id();
        let _ = log.insert(ep_rec.clone())?;
        minted.push(ep_rec);
        for &recipient in &everyone_recipients {
            let wrap_rec = log
                .draft_wrap(author.actor, author.authored_ms, ep_id, &ep_key, recipient)?
                .sign(author.writer_seed, author.incarnation)?;
            let _ = log.insert(wrap_rec.clone())?;
            minted.push(wrap_rec);
        }
        if let Some(everyone_entries) = opened.by_audience.get(&Audience::Everyone) {
            for entry in everyone_entries.values() {
                let put_rec = log
                    .draft_put(author.actor, author.authored_ms, ep_id, &ep_key, entry)?
                    .sign(author.writer_seed, author.incarnation)?;
                let _ = log.insert(put_rec.clone())?;
                minted.push(put_rec);
            }
        }
    }

    let next_commons_gen = opened
        .max_generation
        .get(&EpochAudience::Commons)
        .copied()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let (ep_draft, ep_key) = log.draft_epoch(
        author.actor,
        author.authored_ms,
        EpochAudience::Commons,
        next_commons_gen,
    )?;
    let ep_rec = ep_draft.sign(author.writer_seed, author.incarnation)?;
    let ep_id = *ep_rec.id();
    let _ = log.insert(ep_rec.clone())?;
    minted.push(ep_rec);
    for &recipient in &commons_recipients {
        let wrap_rec = log
            .draft_wrap(author.actor, author.authored_ms, ep_id, &ep_key, recipient)?
            .sign(author.writer_seed, author.incarnation)?;
        let _ = log.insert(wrap_rec.clone())?;
        minted.push(wrap_rec);
    }
    if let Some(ctrl) = &opened.commons_control {
        let put_rec = log
            .draft_commons_put(
                author.actor,
                author.authored_ms,
                ep_id,
                &ep_key,
                ctrl.as_slice(),
            )?
            .sign(author.writer_seed, author.incarnation)?;
        let _ = log.insert(put_rec.clone())?;
        minted.push(put_rec);
    }
    Ok(())
}

/// Self-close missing [`Body::Wrap`]s for active members (`§3.3.6`), and when `allow_coalesce` is
/// `true` and the log holds concurrent [`EpochAudience::Commons`] epochs (`§3.3.7`), mint one
/// higher `Commons` epoch covering all current heads.
///
/// A Commons exchange calls it inside its own bounded deadline (`aterm-db`'s
/// `sync_log_membership`), so the records it mints go to the vault's source on a thread of their
/// own ([`post_records_aside`]) and it returns without waiting on the network.
///
/// # Errors
/// Local file I/O or record signing fails.
pub fn reconcile_commons_log(paths: &Paths, allow_coalesce: bool) -> crate::Result<Vec<Record>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let Some((minted, _, ())) = author_and_commit_records_until(paths, deadline, |store| {
        Ok((
            draft_reconcile_commons_log(paths, store, allow_coalesce)?,
            (),
        ))
    })?
    else {
        return Ok(Vec::new());
    };
    if !minted.is_empty() {
        post_records_aside(paths, encode_records(&minted));
    }
    Ok(minted)
}

/// [`post_records_best_effort`] on a thread of its own, for a caller inside a bounded exchange:
/// its POST may wait the transport's whole exchange deadline on a source that takes it and does
/// not answer, past the exchange's own. The records are already in this Mac's store, so a POST
/// that never runs (no thread to spare) or fails loses none: the exchange trades them with its
/// peer, and the next records pass re-posts this Mac's store to a server that lacks them
/// (`X-Aterm-Records-Need`).
fn post_records_aside(paths: &Paths, wire: Vec<u8>) {
    let paths = paths.clone();
    let _ = std::thread::Builder::new()
        .name("records-post".to_owned())
        .spawn(move || post_records_best_effort(&paths, &wire));
}

fn draft_reconcile_commons_log(
    paths: &Paths,
    store: &Store,
    allow_coalesce: bool,
) -> crate::Result<Vec<Record>> {
    let Some(mut log) = store.log().cloned() else {
        return Ok(Vec::new());
    };
    if !log.has_commons_epochs() || !commons_log_matches_local_lineage(paths, &log)? {
        return Ok(Vec::new());
    }
    let Some(identity) = super::read_identity(paths)? else {
        return Ok(Vec::new());
    };
    let active_recipients: BTreeSet<Recipient> = log
        .membership()
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect();
    if !log.needs_commons_reconcile(&identity.recipient(), &active_recipients, allow_coalesce) {
        return Ok(Vec::new());
    }
    let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
        return Ok(Vec::new());
    };
    let Some(mut opened) = log.open_audience_entries(&identity)? else {
        return Ok(Vec::new());
    };

    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut minted = Vec::new();
    device.with_seed(|writer_seed| -> crate::Result<()> {
        let author = AuthorCtx {
            actor: &actor,
            writer_seed,
            incarnation,
            authored_ms,
        };
        if allow_coalesce {
            coalesce_concurrent_commons_epochs(
                &mut log,
                &opened,
                &author,
                &active_recipients,
                &mut minted,
            )?;
            if !minted.is_empty() {
                if !log.needs_commons_reconcile(&identity.recipient(), &active_recipients, false) {
                    return Ok(());
                }
                let Some(refreshed) = log.open_audience_entries(&identity)? else {
                    return Ok(());
                };
                opened = refreshed;
            }
        }
        let membership = log.membership();
        let commons_recipients: BTreeSet<Recipient> = membership
            .members()
            .iter()
            .filter(|(m_actor, _)| {
                !log.left_out(&opened, &EpochAudience::Commons, m_actor)
                    && !log.left_out(&opened, &EpochAudience::Everyone, m_actor)
            })
            .map(|(_, m)| m.binding.recipient())
            .collect();
        let everyone_recipients: BTreeSet<Recipient> = membership
            .members()
            .iter()
            .filter(|(m_actor, _)| !log.left_out(&opened, &EpochAudience::Everyone, m_actor))
            .map(|(_, m)| m.binding.recipient())
            .collect();
        wrap_unwrapped_commons_and_everyone(
            &mut log,
            &opened,
            &author,
            &commons_recipients,
            &everyone_recipients,
            &mut minted,
        )?;
        Ok(())
    })?;
    Ok(minted)
}

fn coalesce_concurrent_commons_epochs(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    author: &AuthorCtx<'_>,
    active_recipients: &BTreeSet<Recipient>,
    minted: &mut Vec<Record>,
) -> crate::Result<()> {
    let removed = log.removed_recipients();
    let max_everyone = opened
        .maximal_epochs
        .get(&EpochAudience::Everyone)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let holds_any_everyone = log.records().skip(1).any(|r| {
        matches!(
            r.body(),
            Body::Epoch {
                audience: EpochAudience::Everyone,
                ..
            }
        ) && opened.unwrapped_keys.contains_key(r.id())
    });
    let can_open_all_max_everyone = !max_everyone.is_empty()
        && holds_any_everyone
        && max_everyone.iter().all(|ep_id| {
            opened.unwrapped_keys.contains_key(ep_id)
                || opened
                    .wrapped_recipients
                    .get(ep_id)
                    .is_none_or(|set| set.is_disjoint(active_recipients))
        });
    let everyone_wraps_removed = !removed.is_empty()
        && max_everyone.iter().any(|ep_id| {
            opened
                .wrapped_recipients
                .get(ep_id)
                .is_some_and(|set| !set.is_disjoint(&removed))
        });
    let empty_exclude = BTreeSet::new();
    if can_open_all_max_everyone && (max_everyone.len() > 1 || everyone_wraps_removed) {
        let everyone_recipients = carried_audience_recipients(
            log,
            opened,
            &EpochAudience::Everyone,
            active_recipients,
            &empty_exclude,
        );
        let next_gen = opened
            .max_generation
            .get(&EpochAudience::Everyone)
            .copied()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        let (ep_draft, ep_key) = log.draft_epoch(
            author.actor,
            author.authored_ms,
            EpochAudience::Everyone,
            next_gen,
        )?;
        let ep_rec = ep_draft.sign(author.writer_seed, author.incarnation)?;
        let ep_id = *ep_rec.id();
        let _ = log.insert(ep_rec.clone())?;
        minted.push(ep_rec);
        for &recipient in &everyone_recipients {
            let wrap_rec = log
                .draft_wrap(author.actor, author.authored_ms, ep_id, &ep_key, recipient)?
                .sign(author.writer_seed, author.incarnation)?;
            let _ = log.insert(wrap_rec.clone())?;
            minted.push(wrap_rec);
        }
        if let Some(everyone_entries) = opened.by_audience.get(&Audience::Everyone) {
            for entry in everyone_entries.values() {
                let put_rec = log
                    .draft_put(author.actor, author.authored_ms, ep_id, &ep_key, entry)?
                    .sign(author.writer_seed, author.incarnation)?;
                let _ = log.insert(put_rec.clone())?;
                minted.push(put_rec);
            }
        }
    }

    let max_commons = opened
        .maximal_epochs
        .get(&EpochAudience::Commons)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let holds_any_commons = log.records().skip(1).any(|r| {
        matches!(
            r.body(),
            Body::Epoch {
                audience: EpochAudience::Commons,
                ..
            }
        ) && opened.unwrapped_keys.contains_key(r.id())
    });
    let commons_wraps_removed = !removed.is_empty()
        && max_commons.iter().any(|ep_id| {
            opened
                .wrapped_recipients
                .get(ep_id)
                .is_some_and(|set| !set.is_disjoint(&removed))
        });
    if !holds_any_commons || (max_commons.len() <= 1 && !commons_wraps_removed) {
        return Ok(());
    }
    let commons_recipients = carried_audience_recipients(
        log,
        opened,
        &EpochAudience::Commons,
        active_recipients,
        &empty_exclude,
    );
    let next_gen = opened
        .max_generation
        .get(&EpochAudience::Commons)
        .copied()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let (ep_draft, ep_key) = log.draft_epoch(
        author.actor,
        author.authored_ms,
        EpochAudience::Commons,
        next_gen,
    )?;
    let ep_rec = ep_draft.sign(author.writer_seed, author.incarnation)?;
    let ep_id = *ep_rec.id();
    let _ = log.insert(ep_rec.clone())?;
    minted.push(ep_rec);
    for &recipient in &commons_recipients {
        let wrap_rec = log
            .draft_wrap(author.actor, author.authored_ms, ep_id, &ep_key, recipient)?
            .sign(author.writer_seed, author.incarnation)?;
        let _ = log.insert(wrap_rec.clone())?;
        minted.push(wrap_rec);
    }
    if let Some(ctrl) = &opened.commons_control {
        let put_rec = log
            .draft_commons_put(
                author.actor,
                author.authored_ms,
                ep_id,
                &ep_key,
                ctrl.as_slice(),
            )?
            .sign(author.writer_seed, author.incarnation)?;
        let _ = log.insert(put_rec.clone())?;
        minted.push(put_rec);
    }
    Ok(())
}

enum LogEnrollPlan {
    Existing(RecordId),
    ListedFounder {
        name: String,
        label: String,
        node_key: [u8; 32],
    },
    VaultMachine {
        name: String,
        label: String,
    },
}

fn plan_log_enrollment(
    paths: &Paths,
    log: &Log,
    trust: crate::commons::journal::Trust,
    binding: &crate::commons::binding::Binding,
) -> crate::Result<LogEnrollPlan> {
    if *binding.network() != trust.network || *binding.authority() != trust.authority {
        return Err(Error::new("Commons enrollment has another local root"));
    }
    let membership = log.membership();
    if membership.removal(&binding.actor()).is_some()
        || log.recipient_is_removed(&binding.recipient())
    {
        return Err(Error::new("Commons enrollment recipient was removed"));
    }
    if let Some((actor, existing)) = membership
        .members()
        .iter()
        .find(|(_, m)| m.binding.recipient() == binding.recipient())
    {
        if &existing.binding != binding {
            return Err(Error::new(
                "Commons enrollment cannot replace an existing device",
            ));
        }
        if log.audience_left_out(&EpochAudience::Commons, actor)
            || log.audience_left_out(&EpochAudience::Everyone, actor)
        {
            return Err(Error::new("Commons enrollment recipient is not admitted"));
        }
        return Ok(LogEnrollPlan::Existing(existing.entry));
    }
    if let Some(founder) = membership
        .unproved()
        .iter()
        .find(|f| f.recipient == binding.recipient())
    {
        if !log.wraps_maximal_audience_to(&EpochAudience::Commons, &founder.recipient)
            || log.audience_left_out_recipient(&EpochAudience::Everyone, &founder.recipient)
        {
            return Err(Error::new("Commons enrollment recipient is not admitted"));
        }
        return Ok(LogEnrollPlan::ListedFounder {
            name: founder.name.clone(),
            label: founder.label.clone(),
            node_key: founder.node_key.unwrap_or([0; 32]),
        });
    }
    // Pre-F15 fallback: allow a non-removed, durably admitted recipient listed in the cached
    // vault's `[machines]` when the local member can read it, signing with the member's device
    // key (never authority).
    if let Ok(admin) =
        super::enrollment::admin_cached_until(paths, Instant::now() + Duration::from_secs(5))
        && admin.admissions.permits(trust, &binding.recipient())
        && let Some(machine) = admin
            .published_doc
            .machines()
            .into_iter()
            .find(|m| Recipient::parse(&m.recipient).ok() == Some(binding.recipient()))
        && admin
            .commons_eligibility
            .includes(&machine.name, binding.recipient())
    {
        if log.wraps_to(&binding.recipient())
            && (!log.wraps_maximal_audience_to(&EpochAudience::Commons, &binding.recipient())
                || log.audience_left_out_recipient(&EpochAudience::Everyone, &binding.recipient()))
        {
            return Err(Error::new("Commons enrollment recipient is not admitted"));
        }
        let label = log
            .next_label(&machine.name, &BTreeSet::new(), &BTreeSet::new())
            .ok_or_else(|| Error::new("no m<N> label up to m99999 is free"))?;
        return Ok(LogEnrollPlan::VaultMachine {
            name: machine.name,
            label,
        });
    }
    Err(Error::new("Commons enrollment recipient is not admitted"))
}

/// Preflight check for member-served `ACE1` enrollment against a log-backed Commons installation
/// (`F18`). Any active bound member in the log can serve it; the authority seed is never used.
///
/// # Errors
/// Local custody is not log-backed, this Mac is not an active bound member, or `binding` is
/// ineligible.
pub fn check_log_commons_enrollment(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
    binding: &crate::commons::binding::Binding,
) -> crate::Result<()> {
    let Some((log, _)) = trusted_commons_log(paths, trust)? else {
        return Err(Error::new("this peer has no log-backed Commons custody"));
    };
    let Some((actor, _, _)) = open_local_member_signer(paths, &log)? else {
        return Err(Error::new("this peer is not an active Commons log member"));
    };
    if log.audience_left_out(&EpochAudience::Commons, &actor) {
        return Err(Error::new("this peer is not an active Commons log member"));
    }
    let _ = plan_log_enrollment(paths, &log, trust, binding)?;
    Ok(())
}

/// Complete member-served `ACE1` enrollment against a log-backed Commons installation (`F18`),
/// returning the minimal causal witness record chain (plus any newly signed `Wrap`s) for the
/// enrolled device.
///
/// # Errors
/// Local custody changed, `possession` is ineligible, or signing/retaining records fails.
pub fn enroll_commons_member_in_log(
    paths: &Paths,
    trust: crate::commons::journal::Trust,
    request: crate::commons::binding::Request,
    possession: crate::commons::binding::Possession,
) -> crate::Result<Vec<Record>> {
    if request.binding() != possession.binding() {
        return Err(Error::new(
            "Commons enrollment possession differs from request",
        ));
    }
    let recipient = request.binding().recipient();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (minted, committed, entry_id) =
        author_and_commit_records_until(paths, deadline, |store| {
            draft_enroll_commons_member(paths, store, trust, request.clone())
        })?
        .ok_or_else(|| Error::new("this peer's membership log is busy; try again"))?;
    if !minted.is_empty() {
        post_records_aside(paths, encode_records(&minted));
    }
    let log = committed
        .log()
        .ok_or_else(|| Error::new("this peer has no log-backed Commons custody"))?;
    Ok(enrollment_witness_records(log, entry_id, recipient))
}

fn draft_enroll_commons_member(
    paths: &Paths,
    store: &Store,
    trust: crate::commons::journal::Trust,
    request: crate::commons::binding::Request,
) -> crate::Result<(Vec<Record>, RecordId)> {
    let Some((mut log, _)) = trusted_commons_log_from_store(paths, store, trust)? else {
        return Err(Error::new("this peer has no log-backed Commons custody"));
    };
    let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
        return Err(Error::new("this peer is not an active Commons log member"));
    };
    if log.audience_left_out(&EpochAudience::Commons, &actor) {
        return Err(Error::new("this peer is not an active Commons log member"));
    }
    let identity = super::read_identity(paths)?
        .ok_or_else(|| Error::new("this peer has no secrets identity"))?;
    let Some(opened) = log.open_audience_entries(&identity)? else {
        return Err(Error::new("this peer cannot open membership log epochs"));
    };
    let binding = request.binding().clone();
    let plan = plan_log_enrollment(paths, &log, trust, &binding)?;
    let authored_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut minted = Vec::new();
    let entry_id = device.with_seed(|writer_seed| -> crate::Result<RecordId> {
        let id = match plan {
            LogEnrollPlan::Existing(entry_id) => entry_id,
            LogEnrollPlan::ListedFounder {
                name,
                label,
                node_key,
            } => {
                let entrant = Entrant {
                    request,
                    node_key,
                    name,
                    label,
                };
                let rec = log
                    .draft(
                        &actor,
                        authored_ms,
                        Body::Admit {
                            entrant,
                            evidence: Evidence::Listed,
                        },
                    )
                    .sign(writer_seed, incarnation)?;
                let id = *rec.id();
                let _ = log.insert(rec.clone())?;
                minted.push(rec);
                id
            }
            LogEnrollPlan::VaultMachine { name, label } => {
                let entrant = Entrant {
                    request,
                    node_key: [0; 32],
                    name,
                    label,
                };
                let rec = log
                    .draft(
                        &actor,
                        authored_ms,
                        Body::Admit {
                            entrant,
                            evidence: Evidence::Manual,
                        },
                    )
                    .sign(writer_seed, incarnation)?;
                let id = *rec.id();
                let _ = log.insert(rec.clone())?;
                minted.push(rec);
                id
            }
        };
        let commons_recipients = BTreeSet::from([binding.recipient()]);
        let everyone_recipients = if log.membership().contains(&binding.actor())
            && log.left_out(&opened, &EpochAudience::Everyone, &binding.actor())
        {
            BTreeSet::new()
        } else {
            commons_recipients.clone()
        };
        wrap_unwrapped_commons_and_everyone(
            &mut log,
            &opened,
            &AuthorCtx {
                actor: &actor,
                writer_seed,
                incarnation,
                authored_ms,
            },
            &commons_recipients,
            &everyone_recipients,
            &mut minted,
        )?;
        Ok(id)
    })?;
    Ok((minted, entry_id))
}

fn enrollment_witness_records(log: &Log, entry_id: RecordId, recipient: Recipient) -> Vec<Record> {
    let mut targets = vec![entry_id];
    for rec in log.records().skip(1) {
        match rec.body() {
            Body::Epoch {
                audience: EpochAudience::Commons,
                ..
            }
            | Body::Put {
                audience: EpochAudience::Commons,
                ..
            }
            | Body::Remove { .. } => targets.push(*rec.id()),
            Body::Wrap {
                recipient: target_recipient,
                ..
            } if *target_recipient == recipient => {
                targets.push(*rec.id());
            }
            _ => {}
        }
    }
    log.ancestor_closure(&targets)
}

/// Union `<name>.records` across candidate servers and keep the parentless (`F1`) `0.145` envelope
/// mirror in step with the merged log (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.3, F14).
///
/// Returns `Ok(Some(seq))` when a new mirror envelope was published, or `Ok(None)` when every
/// reachable server already holds the current mirror.
///
/// # Errors
/// Vault configuration or admin credentials cannot be read, or every candidate server rejects the
/// mirror publication.
pub fn admin_mirror_log(paths: &Paths, extra_urls: &[String]) -> crate::Result<Option<u64>> {
    let outcome = mirror_log_to_servers(paths, extra_urls)?;
    if outcome.is_some() {
        let _ = super::sync_without_reconcile(paths, extra_urls);
    }
    Ok(outcome)
}

pub(super) fn mirror_log_to_servers(
    paths: &Paths,
    extra_urls: &[String],
) -> crate::Result<Option<u64>> {
    let Some(config) = super::config(paths)? else {
        return Ok(None);
    };
    if config.signer.is_none() {
        return Ok(None);
    }
    let candidate_urls = ledger_publish::candidate_server_urls(paths, &config, extra_urls);
    if matches!(config.source, Source::Url(_)) || !extra_urls.is_empty() {
        let _ = super::sync_records_with_candidates(paths, None, extra_urls);
        if let Ok(store) = super::load_records(paths)
            && !store.is_empty()
        {
            let wire = store.encode();
            for url in &candidate_urls {
                let _ = transport::post_records(url, &wire);
            }
        }
    }

    let heads = ledger_publish::read_server_heads(paths, extra_urls)?;
    if heads.is_empty() {
        return Ok(None);
    }
    let mut merged = ledger_publish::merge_diverged_heads(&heads)?;
    let identity = super::machine_identity(paths)?;
    let signer_pub = envelope::signer_public(&merged.seed)?;
    let mut log_changed_doc = false;
    let mut import = false;
    if let Some((store, _)) = trusted_seeded_store(paths, &config, &signer_pub, &merged.handoff)?
        && let Some(log) = store.log()
    {
        if log.removes(&identity.recipient()) {
            return Ok(None);
        }
        // The log speaks for the envelope only where it holds every head's records. A head whose
        // records it lacks waits for them: mirroring then would publish the older values it
        // holds. A head a writer with no seat in the log published is imported as it stands.
        let mut covers = true;
        for head in &heads {
            match log_base(Some(log), &head.envelope, &identity)? {
                LogBase::Behind => {
                    // A version this Mac published, whose records it held back when the publish
                    // was cut short: taken in, they let this pass mirror (and send them).
                    let adopted = {
                        let _lock = StateLock::take(paths)?;
                        adopt_held_draft(paths, &head.envelope, &identity)?
                    };
                    if adopted {
                        return mirror_log_to_servers(paths, extra_urls);
                    }
                    return Ok(None);
                }
                LogBase::Covers => {}
                LogBase::Unstamped | LogBase::NoLog => covers = false,
            }
        }
        if covers {
            log_changed_doc = overlay_log_onto_document(log, &identity, &mut merged.doc)?;
            if log_changed_doc {
                if merged.diverged {
                    ledger_publish::reconcile_merged_metadata(&mut merged.doc, &heads)?;
                }
                merged.admissions = merged.admissions.retained(&merged.doc)?;
            }
        } else {
            import = true;
        }
    }
    if !merged.diverged && !log_changed_doc && !import {
        return Ok(None);
    }

    // The records go out before the envelope that stamps them, so no server holds a mirror whose
    // records no server has. Where the log holds every head, an audience this Mac cannot read
    // already holds what each head wrote there, so the merged text is its own base: nothing is
    // written there (re-putting a head's older text would undo the other side's edit). An
    // imported head's text is put whole. A conflict is left for a person, except on an entry the
    // merge of heads settles by its own rule (the swarm policy, roster and lease, Commons control).
    let ruled: BTreeSet<(Audience, String)> = merged
        .doc
        .entries()
        .into_iter()
        .filter(|entry| ledger_publish::ruled_by_merge(&entry.name))
        .map(|entry| (entry.audience, entry.name))
        .collect();
    let drafted = append_document_diff_to_log(
        paths,
        &config,
        &signer_pub,
        &merged.handoff,
        &identity,
        &merged.doc,
        Base {
            doc: (!import).then_some(&merged.doc),
            resolve: Some(&ruled),
        },
    )?;
    let my_recipient = identity.recipient().to_bech32();
    let needs_stamp = import
        && drafted.as_ref().and_then(Drafted::stamp).is_some()
        && merged
            .doc
            .lockers()
            .iter()
            .any(|p| p.recipients.contains(&my_recipient));
    if !merged.diverged
        && !log_changed_doc
        && !needs_stamp
        && drafted.as_ref().is_none_or(|d| d.records.is_empty())
    {
        return Ok(None);
    }
    if let Some(drafted) = &drafted
        && !drafted.records.is_empty()
    {
        let wire = encode_records(&drafted.records);
        for url in &candidate_urls {
            let _ = transport::post_records(url, &wire);
        }
    }

    let mut urls: Vec<String> = heads
        .iter()
        .map(|h| h.url.clone())
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .collect();
    for u in candidate_urls {
        if !urls.contains(&u) {
            urls.push(u);
        }
    }
    if merged.diverged
        && !log_changed_doc
        && !needs_stamp
        && drafted.as_ref().is_none_or(|d| d.records.is_empty())
        && ledger_publish::leader_subsumes_heads(&heads, &merged)
        && let Some(leader) = heads
            .iter()
            .max_by_key(|h| (h.envelope.seq, h.envelope.digest()))
    {
        if !urls.is_empty() {
            let outcomes =
                ledger_publish::publish_to_servers_for_admin(paths, &config, &urls, &leader.raw)?;
            if !outcomes.iter().any(|(_, r)| r.is_ok()) {
                let first_err = outcomes
                    .into_iter()
                    .find_map(|(_, r)| r.err())
                    .unwrap_or_else(|| {
                        Error::new("no candidate server accepted the mirror publish")
                    });
                return Err(first_err);
            }
        } else if matches!(config.source, Source::Path(_)) {
            let _lock = super::configured_admin(paths, &config)?;
            transport::publish(&config.source, &leader.raw)?;
        }
        return Ok(Some(leader.envelope.seq));
    }

    let bytes = super::seal_stamped(
        &config.name,
        merged.seq,
        &merged.seed,
        &merged.commons_seed,
        merged.commons_bound,
        &merged.doc,
        &merged.handoff,
        &merged.admissions,
        drafted.as_ref().and_then(Drafted::stamp).as_deref(),
    )?;

    if !urls.is_empty() {
        let outcomes = ledger_publish::publish_to_servers_for_admin(paths, &config, &urls, &bytes)?;
        if !outcomes.iter().any(|(_, r)| r.is_ok()) {
            let first_err = outcomes
                .into_iter()
                .find_map(|(_, r)| r.err())
                .unwrap_or_else(|| Error::new("no candidate server accepted the mirror publish"));
            return Err(first_err);
        }
    } else if matches!(config.source, Source::Path(_)) {
        let _lock = super::configured_admin(paths, &config)?;
        transport::publish(&config.source, &bytes)?;
    }
    Ok(Some(merged.seq))
}

/// One step of the wrap-gap spread: the member ranked `k` among an epoch's holders closes a gap
/// that is still open `k` steps after this Mac first saw it
/// ([`crate::membership_log::Log::gap_rank`]).
pub(super) const GAP_STEP_MS: u64 = 60_000;

/// The most the `wrap-gaps` mark file holds; a longer one reads as none (every gap seen now).
const MAX_GAP_MARK: usize = 256 * 1024;

/// What [`close_wrap_gaps`] remembers between passes: the records' heads digest it last looked
/// at, when a gap a lower-ranked holder should close is next due here, and when this Mac first
/// saw each gap it holds a rank for, by epoch and recipient.
fn gap_mark_file(paths: &Paths) -> std::path::PathBuf {
    paths.dir.join("wrap-gaps")
}

/// One read of [`gap_mark_file`]: `<digest> <due>` and then `<epoch> <recipient> <first seen>`
/// for each gap.
struct GapMark {
    digest: [u8; 32],
    due: u64,
    seen: BTreeMap<(RecordId, String), u64>,
}

fn read_gap_mark(paths: &Paths) -> Option<GapMark> {
    let bytes = fsx::read_private(&gap_mark_file(paths), MAX_GAP_MARK).ok()??;
    let text = std::str::from_utf8(bytes.as_slice()).ok()?;
    let mut lines = text.lines();
    let (digest, due) = lines.next()?.trim().split_once(' ')?;
    let mut out = [0u8; 32];
    (digest.len() == 64 && aterm_codec::hex::decode_into(digest.as_bytes(), &mut out))
        .then_some(())?;
    let mut seen = BTreeMap::new();
    for line in lines {
        let mut words = line.split_ascii_whitespace();
        let (Some(epoch), Some(recipient), Some(at), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            continue;
        };
        let mut id = [0u8; 32];
        if epoch.len() != 64 || !aterm_codec::hex::decode_into(epoch.as_bytes(), &mut id) {
            continue;
        }
        if let Ok(at) = at.parse() {
            seen.insert((id, recipient.to_owned()), at);
        }
    }
    Some(GapMark {
        digest: out,
        due: due.parse().ok()?,
        seen,
    })
}

fn write_gap_mark(
    paths: &Paths,
    digest: &[u8; 32],
    due: u64,
    seen: &BTreeMap<(RecordId, String), u64>,
) -> crate::Result<()> {
    let mut text = format!("{} {due}\n", aterm_codec::hex::encode_lower(digest));
    for ((epoch, recipient), at) in seen {
        text.push_str(&format!(
            "{} {recipient} {at}\n",
            aterm_codec::hex::encode_lower(epoch)
        ));
    }
    fsx::write_atomic(&gap_mark_file(paths), text.as_bytes(), 0o600)
}

/// Close the wrap gaps this member can close now (§3.3.6, F15): for `[everyone]` and the Commons
/// audience, each epoch it holds that an effective member lacks although the epoch's minter could
/// not have known that member ([`Log::wrap_gaps`]). Of the members that hold the epoch, the one
/// ranked `k` ([`Log::gap_rank`]) signs the `Wrap` once `now_ms` is `k` [`GAP_STEP_MS`] past the
/// time this Mac first saw the gap, so after a heal one member closes each gap instead of all
/// ~35. The spread counts from first sight, not from when the gap opened: after a split longer
/// than a few steps every holder would otherwise be past due at its first look, and all sign.
/// The records' heads digest gates the work: with no new record and nothing due, it reads
/// nothing more. It looks without the state lock first, which the desk, the subscriber and the
/// Commons runtime wait on, and takes it only to sign, looking again under it. Returns the
/// `Wrap`s it signed, already in this Mac's records.
///
/// # Errors
/// Local state cannot be read or written, or a record fails to open or sign.
pub(super) fn close_wrap_gaps(paths: &Paths, now_ms: u64) -> crate::Result<Vec<Record>> {
    if !wrap_gaps_pass(paths, now_ms, false)?.0 {
        return Ok(Vec::new());
    }
    let _lock = StateLock::take(paths)?;
    Ok(wrap_gaps_pass(paths, now_ms, true)?.1)
}

/// One look at the wrap gaps for [`close_wrap_gaps`]: whether one is due for this member now,
/// and, where `sign` (the caller holds the state lock), the `Wrap`s it signed and kept. Each look
/// writes the gap mark with when this Mac first saw each gap it holds a rank for, so the next
/// pass counts from there, and the next pass with no new record and nothing due stops at the
/// digest.
fn wrap_gaps_pass(paths: &Paths, now_ms: u64, sign: bool) -> crate::Result<(bool, Vec<Record>)> {
    let nothing = Ok((false, Vec::new()));
    let Some(config) = super::config(paths)? else {
        return nothing;
    };
    let Some(pin) = config.signer else {
        return nothing;
    };
    let Some((mut store, digest)) = trusted_seeded_store(paths, &config, &pin, &[])? else {
        return nothing;
    };
    let earlier = match read_gap_mark(paths) {
        Some(mark) if mark.digest == digest && (mark.due == 0 || now_ms < mark.due) => {
            return nothing;
        }
        Some(mark) => mark.seen,
        None => BTreeMap::new(),
    };
    let Some(mut log) = store.log().cloned() else {
        return nothing;
    };
    let none_seen = BTreeMap::new();
    let (Some((actor, device, incarnation)), Some(identity)) = (
        open_local_member_signer(paths, &log)?,
        read_identity(paths)?,
    ) else {
        write_gap_mark(paths, &digest, 0, &none_seen)?;
        return nothing;
    };
    let Some(opened) = log.open_audience_entries(&identity)? else {
        write_gap_mark(paths, &digest, 0, &none_seen)?;
        return nothing;
    };
    let audiences = crate::membership_log::admission_audiences(&opened);
    let mut due_now = Vec::new();
    let mut next_due: Option<u64> = None;
    let mut seen = BTreeMap::new();
    for gap in log.wrap_gaps(&opened, &audiences) {
        let Some(rank) = log.gap_rank(&opened, &gap, &actor) else {
            continue;
        };
        let key = (gap.epoch, gap.recipient.to_bech32());
        // A clock set back since never puts first sight in the future.
        let first = earlier.get(&key).copied().unwrap_or(now_ms).min(now_ms);
        seen.insert(key, first);
        let due = first
            .saturating_add(GAP_STEP_MS.saturating_mul(u64::try_from(rank).unwrap_or(u64::MAX)));
        if now_ms < due {
            next_due = Some(next_due.map_or(due, |at| at.min(due)));
            continue;
        }
        due_now.push(gap);
    }
    if due_now.is_empty() {
        write_gap_mark(paths, &digest, next_due.unwrap_or(0), &seen)?;
        return nothing;
    }
    if !sign {
        // Due now: the signing look under the lock reads past this mark.
        write_gap_mark(paths, &digest, now_ms.max(1), &seen)?;
        return Ok((true, Vec::new()));
    }
    let mut signed = Vec::with_capacity(due_now.len());
    device.with_seed(|seed| -> crate::Result<()> {
        for gap in &due_now {
            let key = &opened.unwrapped_keys[&gap.epoch];
            let wrap = log
                .draft_wrap(&actor, now_ms, gap.epoch, key, gap.recipient)?
                .sign(seed, incarnation)?;
            log.insert(wrap.clone())?;
            signed.push(wrap);
        }
        Ok(())
    })?;
    for gap in &due_now {
        seen.remove(&(gap.epoch, gap.recipient.to_bech32()));
    }
    store.ingest(&encode_records(&signed))?;
    fsx::write_atomic(&paths.records_file(), &store.encode(), 0o600)?;
    // The digest changed with the new records, so the next pass looks again, with first sight
    // kept for the gaps still open.
    write_gap_mark(paths, &digest, next_due.unwrap_or(0), &seen)?;
    Ok((true, signed))
}

/// [`close_wrap_gaps`] at the wall clock, and the `Wrap`s it signed posted to every vault server
/// this Mac reaches ([`super::post_records_to_servers`]): the subscriber's pass after it pulls
/// records. Best effort: a pass that fails here tries again on the next.
pub(super) fn tend_wrap_gaps(paths: &Paths) {
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    if close_wrap_gaps(paths, now_ms).is_ok_and(|signed| !signed.is_empty()) {
        let _ = super::post_records_to_servers(paths);
    }
}
