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

#[cfg(test)]
#[path = "log_entries_commit_tests.rs"]
mod commit_tests;

#[cfg(test)]
#[path = "log_entries/held_draft_tests.rs"]
mod held_draft_tests;

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
    if !super::pin_admitted(config, None) {
        return Ok(None);
    }
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
    if !super::pin_admitted(config, None) {
        return Ok(None);
    }
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
    let Some(cached) = super::read_held_copy(paths, config, &state)? else {
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
    if !super::pin_admitted(&config, None) {
        return Err(Error::new(
            "log-backed member configuration pins a retired vault signer",
        ));
    }
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
        || recipient_denied_by_roster_or_log(None, log, &me)
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
    type Observation = (([u8; 32], Recipient), Stamp);
    static STAMPS: std::sync::Mutex<std::collections::VecDeque<Observation>> =
        std::sync::Mutex::new(std::collections::VecDeque::new());
    let key = (envelope.digest(), identity.recipient());
    if let Ok(cache) = STAMPS.lock()
        && let Some((_, stamp)) = cache.iter().find(|(held, _)| held == &key)
    {
        return Ok(stamp.clone());
    }
    let stamp = open_envelope_stamp(envelope, identity)?;
    // Heads are public record identifiers, never a decryption key or a grant.
    // `log_base` still compares them with the current trusted log on every use.
    if let Ok(mut cache) = STAMPS.lock() {
        if cache.len() >= 16 {
            cache.pop_front();
        }
        cache.push_back((key, stamp.clone()));
    }
    Ok(stamp)
}

fn open_envelope_stamp(envelope: &Envelope, identity: &Identity) -> crate::Result<Stamp> {
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
    log.admits_without_locker(identity)
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
                || recipient_denied_by_roster_or_log(None, log, &identity.recipient())
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
    open_entries_preferring_log_selected(paths, config, envelope, identity, None)
}

pub(super) fn open_named_entries_preferring_log(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    identity: &Identity,
    names: &[&str],
) -> crate::Result<OpenedWithDigest> {
    open_entries_preferring_log_selected(paths, config, envelope, identity, Some(names))
}

fn open_entries_preferring_log_selected(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    identity: &Identity,
    names: Option<&[&str]>,
) -> crate::Result<OpenedWithDigest> {
    let seeded = trusted_seeded_log(paths, config, envelope)?;
    open_entries_with_log(
        seeded.as_ref().map(|(log, digest)| (log, *digest)),
        envelope,
        identity,
        names,
    )
}

fn open_entries_with_log(
    seeded: Option<(&Log, [u8; 32])>,
    envelope: &Envelope,
    identity: &Identity,
    names: Option<&[&str]>,
) -> crate::Result<OpenedWithDigest> {
    if let Some((log, digest)) = seeded
        && (log.removes(&identity.recipient())
            || recipient_denied_by_roster_or_log(None, log, &identity.recipient())
            || (log_base(Some(log), envelope, identity)? != LogBase::Behind
                && log
                    .audience_left_out_recipient(&EpochAudience::Everyone, &identity.recipient())))
    {
        return Ok((None, Some(digest)));
    }
    if let Some((log, digest)) = seeded
        && log_base(Some(log), envelope, identity)? == LogBase::Covers
        && reads_log(log, identity)
    {
        let entries = match names {
            Some(names) => log.open_entries_named(identity, names)?,
            None => log.open_entries(identity)?,
        };
        return Ok((entries, Some(digest)));
    }
    // An envelope still validates its complete locker view before selection. This only
    // limits the returned entries; it does not make envelope parsing a selective read.
    let entries = super::open_lockers(envelope, identity)?.map(|mut entries| {
        if let Some(names) = names {
            entries.retain(|entry| names.contains(&entry.name.as_str()));
        }
        entries
    });
    Ok((entries, None))
}

#[derive(Clone, Default)]
pub(super) struct RosterRead {
    pub(super) visible: bool,
    roster: Option<crate::swarm_roster::SwarmRoster>,
    parse_error: Option<String>,
    log_digest: Option<[u8; 32]>,
}

impl RosterRead {
    fn from_entries(entries: Option<Vec<Entry>>) -> Self {
        let Some(entries) = entries else {
            return Self::default();
        };
        let roster = entries
            .into_iter()
            .find(|entry| {
                entry.name == crate::swarm_roster::ROSTER_ENTRY
                    && !entry.if_absent
                    && entry.anchor.is_none()
            })
            .map(|entry| {
                let text = std::str::from_utf8(entry.value.as_slice()).map_err(|_| {
                    Error::new(format!(
                        "{} is not UTF-8",
                        crate::swarm_roster::ROSTER_ENTRY
                    ))
                })?;
                crate::swarm_roster::SwarmRoster::from_toml(text)
            })
            .transpose();
        let (roster, parse_error) = match roster {
            Ok(roster) => (roster, None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            visible: true,
            roster,
            parse_error,
            log_digest: None,
        }
    }

    pub(super) fn into_roster(self) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
        match self.parse_error {
            Some(error) => Err(Error::new(error)),
            None => Ok(self.roster),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum RosterSource {
    Log {
        network: RecordId,
        heads: [u8; 32],
    },
    Envelope {
        envelope: [u8; 32],
        log: Option<(RecordId, [u8; 32])>,
    },
}

/// Only the public roster survives opening the encrypted entries. The exact
/// ciphertexts, held causal prefix and reader identify this pure result; no
/// private key, epoch key, secret entry or source authorization is retained.
/// Callers still establish current source binding before every lookup.
fn observed_roster(
    source: RosterSource,
    identity: &Identity,
    open: impl FnOnce() -> crate::Result<RosterRead>,
) -> crate::Result<RosterRead> {
    type Observation = ((Recipient, RosterSource), RosterRead);
    static ROSTERS: std::sync::Mutex<std::collections::VecDeque<Observation>> =
        std::sync::Mutex::new(std::collections::VecDeque::new());
    let key = (identity.recipient(), source);
    if let Ok(cache) = ROSTERS.lock()
        && let Some((_, result)) = cache.iter().find(|(held, _)| held == &key)
    {
        return Ok(result.clone());
    }
    let result = open()?;
    if let Ok(mut cache) = ROSTERS.lock() {
        if cache.len() >= 16 {
            cache.pop_front();
        }
        cache.push_back((key, result.clone()));
    }
    Ok(result)
}

pub(super) fn roster_preferring_log(
    paths: &Paths,
    config: &Config,
    envelope: &Envelope,
    identity: &Identity,
) -> crate::Result<(RosterRead, Option<[u8; 32]>)> {
    let seeded = trusted_seeded_log(paths, config, envelope)?;
    roster_with_log(
        seeded.as_ref().map(|(log, digest)| (log, *digest)),
        envelope,
        identity,
    )
}

fn roster_with_log(
    seeded: Option<(&Log, [u8; 32])>,
    envelope: &Envelope,
    identity: &Identity,
) -> crate::Result<(RosterRead, Option<[u8; 32]>)> {
    let result = observed_roster(
        RosterSource::Envelope {
            envelope: envelope.digest(),
            log: seeded.map(|(log, digest)| (*log.network(), digest)),
        },
        identity,
        || {
            let (entries, digest) = open_entries_with_log(
                seeded,
                envelope,
                identity,
                Some(&[crate::swarm_roster::ROSTER_ENTRY]),
            )?;
            let mut result = RosterRead::from_entries(entries);
            result.log_digest = digest;
            Ok(result)
        },
    )?;
    let log_digest = result.log_digest;
    Ok((result, log_digest))
}

pub(super) fn roster_from_log(log: &Log, identity: &Identity) -> crate::Result<RosterRead> {
    observed_roster(
        RosterSource::Log {
            network: *log.network(),
            heads: records_heads_digest(log),
        },
        identity,
        || {
            Ok(RosterRead::from_entries(log.open_entries_named(
                identity,
                &[crate::swarm_roster::ROSTER_ENTRY],
            )?))
        },
    )
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

/// Overlay Commons membership and `aterm-commons-control-v1` from `log` onto `doc` without
/// touching ordinary secret entries. Returns whether `doc` changed.
pub(super) fn overlay_log_commons_onto_document(
    log: &Log,
    identity: &Identity,
    doc: &mut Document,
    readmit: Option<&BTreeSet<Recipient>>,
) -> crate::Result<bool> {
    let Some(opened) = log.open_audience_entries(identity)? else {
        return Ok(false);
    };
    overlay_log_commons_with_opened(log, &opened, doc, readmit)
}

/// [`overlay_log_commons_onto_document`] with the audiences already opened; a Mac `readmit` names
/// ([`Base::readmit`]) stays in `doc` and in Commons control although the log left it out.
fn overlay_log_commons_with_opened(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    doc: &mut Document,
    readmit: Option<&BTreeSet<Recipient>>,
) -> crate::Result<bool> {
    let mut changed = false;
    let roster = roster_from_doc_or_opened(Some(doc), Some(opened), Some(log));
    let initial_doc_recipients: BTreeSet<Recipient> = doc
        .machines()
        .into_iter()
        .filter_map(|m| Recipient::parse(&m.recipient).ok())
        .collect();
    let mut admins: BTreeSet<String> = doc.admins().into_iter().collect();
    for machine in doc.machines() {
        if admins.contains(&machine.name) && admins.len() <= 1 {
            continue;
        }
        if let Ok(recipient) = Recipient::parse(&machine.recipient)
            && (log.recipient_is_removed(&recipient)
                || kept_out(log, &EpochAudience::Everyone, &recipient, readmit)
                || machine_denied_by_roster_or_log(roster.as_ref(), log, &machine.name, &recipient))
            && doc.remove_machine(&machine.name, true) == Ok(true)
        {
            admins.remove(&machine.name);
            changed = true;
        }
    }
    if log.has_commons_epochs() {
        let membership = log.membership();
        let code_only: BTreeSet<String> = doc.code_only().into_iter().collect();
        let mut existing_recipients: BTreeSet<Recipient> = doc
            .machines()
            .into_iter()
            .filter_map(|m| Recipient::parse(&m.recipient).ok())
            .collect();
        let mut existing_names: BTreeSet<String> =
            doc.machines().into_iter().map(|m| m.name).collect();
        let mut ordered_members: Vec<(&MemberId, &crate::membership_log::Member)> =
            membership.members().iter().collect();
        ordered_members.sort_by_key(|(actor, member)| {
            std::cmp::Reverse((log.record_index(&member.entry).unwrap_or(0), **actor))
        });
        for (actor, member) in ordered_members {
            let recipient = member.binding.recipient();
            if existing_recipients.contains(&recipient)
                || code_only.contains(&recipient.to_bech32())
                || log.recipient_is_removed(&recipient)
                || membership.member_node_removed(member)
                || log.left_out(opened, &EpochAudience::Everyone, actor)
                || roster
                    .as_ref()
                    .is_some_and(|r| member_denied_by_roster(r, member))
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
            .filter(|(actor, member)| {
                !membership.member_node_removed(member)
                    && !log.left_out(opened, &EpochAudience::Commons, actor)
                    && !log.left_out(opened, &EpochAudience::Everyone, actor)
                    && !roster
                        .as_ref()
                        .is_some_and(|r| member_denied_by_roster(r, member))
            })
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
                    && !kept_out(log, &EpochAudience::Commons, &r, readmit)
                    && !kept_out(log, &EpochAudience::Everyone, &r, readmit)
                    && !machine_denied_by_roster_or_log(roster.as_ref(), log, &m.name, &r)
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
    Ok(changed)
}

/// Overlay the winning entries from `log` across every [`Audience`] fully unwrapped by `identity`
/// onto `doc`, leaving audiences `identity` cannot unwrap untouched; a Mac `readmit` names
/// ([`Base::readmit`]) stays in `doc` although the log left it out. Returns whether `doc` changed.
pub(super) fn overlay_log_onto_document(
    log: &Log,
    identity: &Identity,
    doc: &mut Document,
    readmit: Option<&BTreeSet<Recipient>>,
) -> crate::Result<bool> {
    let Some(opened) = log.open_audience_entries(identity)? else {
        return Ok(false);
    };
    let mut changed = overlay_log_commons_with_opened(log, &opened, doc, readmit)?;

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

/// Whether `member`'s log seat is denied by `roster` (`deny_labels` or `deny_nodes`).
///
/// When `roster` holds a [`SwarmMember`] row for `member`'s recipient, that row's labels/aliases
/// and node identifiers are authoritative; only when no recipient row exists does this fall back
/// to the log seat's `m<N>` label/name.
#[must_use]
pub fn member_denied_by_roster(
    roster: &crate::swarm_roster::SwarmRoster,
    member: &crate::membership_log::Member,
) -> bool {
    let rec_str = member.binding.recipient().to_bech32();
    let node_denied = member.node_key.is_some_and(|nk| {
        nk != [0; 32] && roster.is_node_denied(&aterm_codec::hex::encode_lower(&nk))
    }) || roster
        .is_node_denied(&aterm_codec::hex::encode_lower(member.binding.writer()))
        || roster.is_node_denied(&aterm_codec::hex::encode_lower(
            member.binding.incarnation(),
        ))
        || roster.is_node_denied(&aterm_codec::hex::encode_lower(member.binding.machine()));
    if let Some(row) = roster.member_by_recipient(&rec_str) {
        return super::steward::member_is_denied_in_roster(roster, row) || node_denied;
    }
    let label = crate::swarm_roster::leading_m_label(&member.label)
        .or_else(|| crate::swarm_roster::leading_m_label(&member.name))
        .map(|(_, c)| c);
    label.as_deref().is_some_and(|l| roster.is_label_denied(l)) || node_denied
}

pub(super) fn recipient_denied_by_roster_or_log(
    roster: Option<&crate::swarm_roster::SwarmRoster>,
    log: &Log,
    recipient: &Recipient,
) -> bool {
    let rec_str = recipient.to_bech32();
    let membership = log.membership();
    let newest_seat = membership
        .members()
        .iter()
        .filter(|(_, mem)| mem.binding.recipient() == *recipient)
        .max_by_key(|(actor, mem)| (log.record_index(&mem.entry).unwrap_or(0), *actor))
        .map(|(_, mem)| mem);
    roster.is_some_and(|ros| {
        ros.member_by_recipient(&rec_str)
            .is_some_and(|row| super::steward::member_is_denied_in_roster(ros, row))
    }) || newest_seat.is_some_and(|mem| {
        membership.member_node_removed(mem)
            || roster.is_some_and(|ros| member_denied_by_roster(ros, mem))
    }) || (newest_seat.is_none()
        && membership.unproved().iter().any(|f| {
            f.recipient == *recipient
                && (f.node_key.is_some_and(|nk| {
                    nk != [0; 32]
                        && matches!(
                            membership.node(&nk),
                            crate::membership_log::NodeStanding::Removed(_)
                        )
                }) || roster.is_some_and(|ros| {
                    let node_denied = f.node_key.is_some_and(|nk| {
                        nk != [0; 32] && ros.is_node_denied(&aterm_codec::hex::encode_lower(&nk))
                    });
                    if let Some(row) = ros.member_by_recipient(&rec_str) {
                        return super::steward::member_is_denied_in_roster(ros, row) || node_denied;
                    }
                    let label = crate::swarm_roster::leading_m_label(&f.label)
                        .or_else(|| crate::swarm_roster::leading_m_label(&f.name))
                        .map(|(_, c)| c);
                    label.as_deref().is_some_and(|l| ros.is_label_denied(l)) || node_denied
                }))
        }))
}

pub(super) fn machine_denied_by_roster_or_log(
    roster: Option<&crate::swarm_roster::SwarmRoster>,
    log: &Log,
    host: &str,
    recipient: &Recipient,
) -> bool {
    if recipient_denied_by_roster_or_log(roster, log, recipient) {
        return true;
    }
    let rec_str = recipient.to_bech32();
    let membership = log.membership();
    let has_authoritative_seat = roster
        .is_some_and(|ros| ros.member_by_recipient(&rec_str).is_some())
        || membership
            .members()
            .values()
            .any(|mem| mem.binding.recipient() == *recipient)
        || membership
            .unproved()
            .iter()
            .any(|f| f.recipient == *recipient);
    if has_authoritative_seat {
        return false;
    }
    roster.is_some_and(|ros| {
        ros.is_label_denied(host)
            || ros
                .member_by_label_or_alias(host)
                .is_some_and(|row| super::steward::member_is_denied_in_roster(ros, row))
    })
}

fn merge_swarm_roster(
    acc: &mut crate::swarm_roster::SwarmRoster,
    other: crate::swarm_roster::SwarmRoster,
) {
    acc.deny_labels.extend(other.deny_labels);
    acc.deny_nodes.extend(other.deny_nodes);
    acc.canonicalize();
    for m in other.members {
        if let Some(pos) = acc
            .members
            .iter()
            .enumerate()
            .filter(|(_, row)| row.recipient.eq_ignore_ascii_case(&m.recipient))
            .max_by_key(|(idx, row)| {
                (
                    !acc.is_member_denied(row),
                    row.node_key.is_some() || row.node_id.is_some(),
                    row.admitted_ms,
                    std::cmp::Reverse(*idx),
                )
            })
            .map(|(idx, _)| idx)
        {
            let existing_denied = acc.is_member_denied(&acc.members[pos]);
            let m_denied = acc.is_member_denied(&m);
            if existing_denied && !m_denied {
                acc.members[pos] = m;
                continue;
            }
            if !existing_denied && m_denied {
                continue;
            }
            let existing = &mut acc.members[pos];
            if m.admitted_ms > existing.admitted_ms {
                if m.node_id.is_some() {
                    existing.node_id = m.node_id;
                }
                if m.node_key.is_some() {
                    existing.node_key = m.node_key;
                }
            } else {
                if existing.node_id.is_none() {
                    existing.node_id = m.node_id;
                }
                if existing.node_key.is_none() {
                    existing.node_key = m.node_key;
                }
            }
            if existing.label != m.label && !existing.aliases.contains(&m.label) {
                existing.aliases.push(m.label);
            }
            for alias in m.aliases {
                if !existing.aliases.contains(&alias) {
                    existing.aliases.push(alias);
                }
            }
        } else {
            acc.members.push(m);
        }
    }
    acc.canonicalize();
}

pub(super) fn roster_from_doc_or_opened(
    doc: Option<&Document>,
    opened: Option<&crate::membership_log::entries::OpenedAudienceEntries>,
    log: Option<&Log>,
) -> Option<crate::swarm_roster::SwarmRoster> {
    let opened_entry = opened.and_then(|opened| {
        opened
            .by_audience
            .get(&Audience::Everyone)?
            .get(crate::swarm_roster::ROSTER_ENTRY)
    });
    roster_from_doc_or_entry(doc, opened_entry, log)
}

fn roster_from_doc_or_entry(
    doc: Option<&Document>,
    opened: Option<&Entry>,
    log: Option<&Log>,
) -> Option<crate::swarm_roster::SwarmRoster> {
    let mut combined: Option<crate::swarm_roster::SwarmRoster> = None;
    let doc_entry =
        doc.and_then(|doc| doc.entry(&Audience::Everyone, crate::swarm_roster::ROSTER_ENTRY));
    for entry in [doc_entry, opened].into_iter().flatten() {
        let Ok(text) = std::str::from_utf8(entry.value.as_slice()) else {
            continue;
        };
        let Ok(roster) = crate::swarm_roster::SwarmRoster::from_toml(text) else {
            continue;
        };
        match &mut combined {
            None => combined = Some(roster),
            Some(acc) => merge_swarm_roster(acc, roster),
        }
    }
    if let (Some(log), Some(roster)) = (log, &mut combined) {
        let _ = super::roster::lift_restored_from_roster(log, roster);
    }
    combined
}

pub fn local_swarm_roster_for_log(
    paths: &Paths,
    log: &Log,
) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
    StateLock::narrow_owned(paths)?;
    let lock = StateLock::observe(paths)?.ok_or_else(|| {
        Error::new("the vault state is busy or unavailable while reading its roster")
    })?;
    local_swarm_roster_for_log_held(paths, log, &lock)
}

pub(crate) fn local_swarm_roster_for_log_held(
    paths: &Paths,
    log: &Log,
    lock: &StateLock,
) -> crate::Result<Option<crate::swarm_roster::SwarmRoster>> {
    if !commons_log_matches_local_lineage(paths, log)? {
        return Err(Error::new(
            "the roster log belongs to another configured vault authority",
        ));
    }
    // A held head has all its parents, so retaining every current head retains the
    // current log's whole causal past. A caller's uncommitted draft may extend it.
    if let Some(expected) = super::config(paths)?
        && let Some(current) = trusted_local_log(paths, &expected)?
        && current.heads().iter().any(|head| log.get(head).is_none())
    {
        return Err(Error::new(
            "the caller's membership log is behind the current vault snapshot",
        ));
    }
    let mut combined = super::roster::base_swarm_roster_held(paths, lock)?;
    if let Some(identity) = super::read_identity(paths)?
        && let Some(entries) = log.open_audience_entries_named(
            &identity,
            &Audience::Everyone,
            &[crate::swarm_roster::ROSTER_ENTRY],
        )?
        && let Some(r) = roster_from_doc_or_entry(None, entries.first(), Some(log))
    {
        match &mut combined {
            None => combined = Some(r),
            Some(acc) => merge_swarm_roster(acc, r),
        }
    }
    let mut roster = combined.unwrap_or_default();
    super::roster::overlay_log_membership(log, &mut roster);
    Ok((!roster.members.is_empty()
        || !roster.deny_labels.is_empty()
        || !roster.deny_nodes.is_empty())
    .then_some(roster))
}

pub(super) fn open_local_member_signer(
    paths: &Paths,
    log: &Log,
) -> crate::Result<Option<(MemberId, Device, [u8; 16])>> {
    open_local_member_signer_until(paths, log, None)
}

fn open_local_member_signer_until(
    paths: &Paths,
    log: &Log,
    deadline: Option<std::time::Instant>,
) -> crate::Result<Option<(MemberId, Device, [u8; 16])>> {
    super::publication_current(deadline)?;
    StateLock::narrow_owned(paths)?;
    let lock = StateLock::observe(paths)?.ok_or_else(|| {
        Error::new("the vault state is busy or unavailable while reading its signer")
    })?;
    open_local_member_signer_held_until(paths, log, &lock, deadline)
}

pub(super) fn open_local_member_signer_held(
    paths: &Paths,
    log: &Log,
    lock: &StateLock,
) -> crate::Result<Option<(MemberId, Device, [u8; 16])>> {
    open_local_member_signer_held_until(paths, log, lock, None)
}

fn open_local_member_signer_held_until(
    paths: &Paths,
    log: &Log,
    lock: &StateLock,
    deadline: Option<std::time::Instant>,
) -> crate::Result<Option<(MemberId, Device, [u8; 16])>> {
    super::publication_current(deadline)?;
    if fsx::kind(&paths.dir.join("commons").join("device.age"))? != fsx::Kind::File {
        return Ok(None);
    }
    let device = match deadline.map_or_else(
        || Device::open_existing(paths),
        |deadline| Device::open_existing_until(paths, deadline),
    ) {
        Ok(device) => device,
        Err(error) => {
            super::publication_current(deadline)?;
            return Err(error);
        }
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
    if &member.binding != req.binding()
        || log.recipient_is_removed(&member.binding.recipient())
        || membership.member_node_removed(member)
        || log.audience_left_out(&EpochAudience::Everyone, &actor)
        || log.audience_left_out_recipient(&EpochAudience::Everyone, &member.binding.recipient())
        || super::steward::local_recipient_denial_held(paths, lock)?
    {
        return Ok(None);
    }
    let config = super::config(paths)?;
    if local_swarm_roster_for_log_held(paths, log, lock)?.is_some_and(|roster| {
        let rec_str = member.binding.recipient().to_bech32();
        member_denied_by_roster(&roster, member)
            || (roster.member_by_recipient(&rec_str).is_none()
                && crate::swarm_roster::leading_m_label(&member.label).is_none()
                && config
                    .as_ref()
                    .is_some_and(|cfg| roster.is_label_denied(&cfg.host)))
    }) {
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
    roster: Option<&crate::swarm_roster::SwarmRoster>,
) -> BTreeSet<Recipient> {
    let code_only: BTreeSet<String> = doc.code_only().into_iter().collect();
    membership
        .members()
        .values()
        .filter(|member| {
            !membership
                .removed_recipients()
                .contains(&member.binding.recipient())
                && !membership.member_node_removed(member)
                && !roster.is_some_and(|r| member_denied_by_roster(r, member))
        })
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

/// Records a writer drafted for a document and has not kept yet, and the log's heads once they are
/// in: what the envelope sealed with them stamps ([`stamp_line`]).
#[derive(Clone)]
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
/// add the records to `paths.records_file()`, then send the whole local store to `urls`, every
/// one of them at once and by `posts_by` (one exchange from now where none is given,
/// [`super::fan_out::post_records_to`]): a server that takes the post and never reads it holds
/// the command whose version has landed for that one deadline, not one each.
///
/// # Errors
/// The complete draft cannot be retained, or the records file cannot be read or written.
pub(super) fn keep_drafted_until(
    paths: &Paths,
    drafted: &Drafted,
    urls: &[String],
    deadline: Option<std::time::Instant>,
    posts_by: Option<std::time::Instant>,
) -> crate::Result<()> {
    if !drafted.records.is_empty() {
        let _lock = match deadline {
            Some(deadline) => StateLock::take_at_until(paths, &paths.lock_file(), deadline)?,
            None => StateLock::take(paths)?,
        };
        let mut current = super::load_records(paths)?;
        // The envelope already names these exact signed records. Peer ingress
        // may have consumed their room since drafting; a partial keep must not
        // let the publisher discard its only complete recovery copy.
        commit_authored_records(
            paths,
            &mut current,
            &encode_records(&drafted.records),
            &drafted.records,
        )?;
    }
    if urls.is_empty() || deadline.is_some() {
        // The normal subscriber transfers this durable log. A bounded caller
        // must not start best-effort post-publication network work here.
        return Ok(());
    }
    let store = super::load_records(paths)?;
    if store.is_empty() {
        return Ok(());
    }
    let by = posts_by.unwrap_or_else(super::fan_out::one_exchange);
    let _ = super::fan_out::post_records_to(urls, &store.encode(), by);
    Ok(())
}

/// The recipients of each locker `doc` seals, by audience, less any `log` removed or left out of
/// `[everyone]` (unless `readmit` lets it back in).
fn plan_recipients_of(
    doc: &Document,
    log: &Log,
    readmit: Option<&BTreeSet<Recipient>>,
) -> crate::Result<BTreeMap<Audience, BTreeSet<Recipient>>> {
    let mut out = BTreeMap::new();
    for plan in doc.lockers() {
        let mut recipients = BTreeSet::new();
        for text in &plan.recipients {
            let recipient = Recipient::parse(text).map_err(|_| Error::new("bad machine key"))?;
            if !log.recipient_is_removed(&recipient)
                && !kept_out(log, &EpochAudience::Everyone, &recipient, readmit)
                && !recipient_denied_by_roster_or_log(None, log, &recipient)
            {
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

/// Whether a writer puts the Commons control descriptor `doc` its text carries (F21 rule 7). Where
/// it opens all of Commons (`whole`), its overlay keeps the text's descriptor current, so it puts
/// it under a fresh epoch (`fresh`) and otherwise when it differs from the one it reads (`view`).
/// Where it does not, its overlay leaves the text's descriptor as it was, which can be older than
/// the one every Mac reads; re-putting it, with theirs in its past, would revert theirs. So it
/// puts it only when the person changed it: it differs from `base`'s, the descriptor the edit
/// started from (`None` for a text imported with no base, where no change can be told), and from
/// `view`.
pub(super) fn commons_descriptor_due(
    whole: bool,
    fresh: bool,
    view: Option<&[u8]>,
    doc: &[u8],
    base: Option<Option<&[u8]>>,
) -> bool {
    let differs = |other: Option<&[u8]>| other.is_none_or(|cur| !aterm_digest::ct_eq(cur, doc));
    if whole {
        fresh || differs(view)
    } else {
        base.is_some_and(differs) && differs(view)
    }
}

/// What a writer that seals an audience under a fresh epoch puts there (F21 rule 7): each entry of
/// `next`, what it means the audience to hold, where it opens the whole audience (`whole`); else
/// only those that differ from `view`, what it reads there. A re-put of what it reads, with writes
/// it cannot open in its past, would read as a deliberate write on every Mac that opens them and
/// revert them there. Beside them, each name of `view` that `next` dropped, for a
/// [`Body::Delete`].
fn reput<'a>(
    whole: bool,
    view: Option<&'a BTreeMap<String, Entry>>,
    next: &'a BTreeMap<String, Entry>,
) -> (Vec<&'a Entry>, Vec<&'a String>) {
    let puts = next
        .iter()
        .filter(|(name, entry)| {
            whole
                || view
                    .and_then(|v| v.get(*name))
                    .is_none_or(|cur| !same_entry(cur, entry))
        })
        .map(|(_, entry)| entry)
        .collect();
    let dropped = view
        .into_iter()
        .flat_map(BTreeMap::keys)
        .filter(|name| !next.contains_key(*name))
        .collect();
    (puts, dropped)
}

/// What a writer that holds no key of `audience` drafts there, against `base`, the text the edit
/// started from: a fresh epoch wrapped to `recipients`, a [`Body::Put`] of each of `puts` and a
/// [`Body::Delete`] of each of `dropped` under it.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "KeyedEpochs",
        action = "PartialMint",
        project = "aterm_secrets::client::remove::tests::keyed_epochs_transition"
    )
)]
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
    /// Macs this write lets back in although the log left them out of `[everyone]` or Commons:
    /// approvals this Mac made on the lower-seq side of a partition, which every server dropped
    /// at the heal ([`super::own_head`]). The log's removals and denials still keep them out.
    pub(super) readmit: Option<&'a BTreeSet<Recipient>>,
}

/// Whether `log` leaves `recipient` out of `audience` on purpose, unless `readmit` lets it back
/// in ([`Base::readmit`]).
fn kept_out(
    log: &Log,
    audience: &EpochAudience,
    recipient: &Recipient,
    readmit: Option<&BTreeSet<Recipient>>,
) -> bool {
    readmit.is_none_or(|readmit| !readmit.contains(recipient))
        && log.audience_left_out_recipient(audience, recipient)
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
    draft_document_diff_until(
        paths,
        config,
        envelope_signer,
        handoff,
        identity,
        doc,
        base,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the existing document-diff inputs plus a caller-owned deadline"
)]
pub(super) fn draft_document_diff_until(
    paths: &Paths,
    config: &Config,
    envelope_signer: &[u8; 32],
    handoff: &[([u8; 32], [u8; 64])],
    identity: &Identity,
    doc: &Document,
    base: Base<'_>,
    deadline: Option<std::time::Instant>,
) -> crate::Result<Option<Drafted>> {
    super::publication_current(deadline)?;
    let Some((store, _)) = trusted_seeded_store(paths, config, envelope_signer, handoff)? else {
        return Ok(None);
    };
    draft_document_diff_in_store(paths, &store, identity, doc, base, deadline)
}

/// The document planner over an already authenticated, state-held snapshot.
/// A removal plans its retirement first, then diffs the final publication against
/// that same in-memory store, without publishing or reloading the earlier store.
pub(super) fn draft_document_diff_in_store(
    paths: &Paths,
    store: &Store,
    identity: &Identity,
    doc: &Document,
    base: Base<'_>,
    deadline: Option<std::time::Instant>,
) -> crate::Result<Option<Drafted>> {
    super::publication_current(deadline)?;
    let Some(mut log) = store.log().cloned() else {
        return Ok(None);
    };
    let Some((actor, device, incarnation)) = open_local_member_signer_until(paths, &log, deadline)?
    else {
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
    // The descriptor the edit started from: `None` with no `base.doc`, `Some(None)` where it had
    // none.
    let base_commons_value: Option<Option<SecretBytes>> = base.doc.map(|text| {
        text.entries()
            .into_iter()
            .find(|e| e.name == crate::commons::ENTRY)
            .map(|e| e.value)
    });
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
    let roster = roster_from_doc_or_opened(Some(doc), Some(&opened), Some(&log));
    let log_recipients = log_member_recipients(&membership, doc, roster.as_ref());
    let everyone_log_recipients: BTreeSet<Recipient> = membership
        .members()
        .iter()
        .filter(|(m_actor, m)| {
            !log.recipient_is_removed(&m.binding.recipient())
                && !membership.member_node_removed(m)
                && !log.left_out(&opened, &EpochAudience::Everyone, m_actor)
                && !roster
                    .as_ref()
                    .is_some_and(|r| member_denied_by_roster(r, m))
        })
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
    // The audiences this Mac opens at least one epoch of, counting or keyed.
    let opened_audiences: BTreeSet<EpochAudience> = opened
        .unwrapped_keys
        .keys()
        .chain(opened.keyed_keys.keys())
        .filter_map(|id| match log.get(id).map(Record::body) {
            Some(Body::Epoch { audience, .. }) => Some(audience.clone()),
            _ => None,
        })
        .collect();
    let mut base_recipients = base
        .doc
        .map(|base_doc| plan_recipients_of(base_doc, &log, base.readmit))
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
                if !log.recipient_is_removed(&recipient)
                    && !kept_out(&log, &EpochAudience::Everyone, &recipient, base.readmit)
                    && !recipient_denied_by_roster_or_log(None, &log, &recipient)
                {
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
                // New or departed machines: a fresh epoch for the list as it is. Only an outsider
                // of a host list, which opens none of its epochs and whose new machines no wrap
                // gap reaches, seals every entry again there; a Mac that opens some reads a view
                // that can miss writes it cannot open, and seals again only what the edit changed
                // (F21 rule 7).
                let resealed = base_recipients.get(&plan.audience) != Some(&plan_recipients);
                let whole = resealed
                    && matches!(plan.audience, Audience::Hosts(_))
                    && !opened_audiences.contains(&EpochAudience::from(&plan.audience));
                let puts: Vec<&Entry> = now
                    .values()
                    .filter(|e| whole || before.get(&e.name).is_none_or(|b| !same_entry(b, e)))
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
                // Every entry goes under the fresh epoch only where this Mac opens the whole
                // audience and every keyed epoch there; else what differs from what it reads
                // (F21 rule 7, `reput`).
                let whole = opened.reputs_whole(&ep_aud);
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
                        let resolve = resolving(&plan.audience, name);
                        if resolve {
                            let del_rec = log
                                .draft_delete(&actor, authored_ms, epoch_id, &epoch_key, name)?
                                .sign(writer_seed, incarnation)?;
                            let _ = log.insert(del_rec.clone())?;
                            minted.push(del_rec);
                        }
                        let unchanged = !whole
                            && !resolve
                            && log_map
                                .and_then(|m| m.get(name))
                                .is_some_and(|cur| same_entry(cur, entry));
                        if unchanged {
                            continue;
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
                .filter(|(m_actor, m)| {
                    !log.recipient_is_removed(&m.binding.recipient())
                        && !membership.member_node_removed(m)
                        && !log.left_out(&opened, &EpochAudience::Commons, m_actor)
                        && !log.left_out(&opened, &EpochAudience::Everyone, m_actor)
                        && !roster
                            .as_ref()
                            .is_some_and(|r| member_denied_by_roster(r, m))
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
                        && !kept_out(&log, &EpochAudience::Commons, &r, base.readmit)
                        && !kept_out(&log, &EpochAudience::Everyone, &r, base.readmit)
                        && !machine_denied_by_roster_or_log(roster.as_ref(), &log, host, &r)
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
                    if commons_descriptor_due(
                        opened
                            .fully_unwrapped_audiences
                            .contains(&EpochAudience::Commons),
                        false,
                        opened.commons_control.as_ref().map(SecretBytes::as_slice),
                        ctrl_entry.value.as_slice(),
                        base_commons_value
                            .as_ref()
                            .map(|value| value.as_ref().map(SecretBytes::as_slice)),
                    ) {
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
                    // The descriptor again only where this Mac opens all of Commons, or where
                    // the person changed it (F21 rule 7, `commons_descriptor_due`).
                    if commons_descriptor_due(
                        opened
                            .fully_unwrapped_audiences
                            .contains(&EpochAudience::Commons),
                        true,
                        opened.commons_control.as_ref().map(SecretBytes::as_slice),
                        ctrl_entry.value.as_slice(),
                        base_commons_value
                            .as_ref()
                            .map(|value| value.as_ref().map(SecretBytes::as_slice)),
                    ) {
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
            .filter(|r| {
                !log.recipient_is_removed(r)
                    && !kept_out(&log, &EpochAudience::Everyone, r, base.readmit)
                    && !recipient_denied_by_roster_or_log(None, &log, r)
            })
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

pub(super) fn trusted_commons_log_from_store(
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

pub(super) struct RecordsAuthorLock(std::fs::File);

impl RecordsAuthorLock {
    fn open_lock_file(paths: &Paths) -> crate::Result<(std::path::PathBuf, std::fs::File)> {
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.dir.join(".records.author.lock");
        let file = fsx::open_lock(&path)?;
        Ok((path, file))
    }

    pub(super) fn take(paths: &Paths) -> crate::Result<Self> {
        let (path, raw) = Self::open_lock_file(paths)?;
        let file: std::fs::File = raw;
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of RecordsAuthorLock, whose Drop releases by LOCK_UN (File::unlock)"
        )]
        file.lock()
            .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        Ok(Self(file))
    }

    fn try_take(paths: &Paths) -> crate::Result<Option<Self>> {
        let (path, raw) = Self::open_lock_file(paths)?;
        let file: std::fs::File = raw;
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

    pub(super) fn try_take_until(paths: &Paths, deadline: Instant) -> crate::Result<Option<Self>> {
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
/// `paths.records_file()` while signing. A batch that cannot be held whole is not written.
pub(super) fn author_and_commit_records<T>(
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
        #[cfg(test)]
        author_batch_tests::before_commit();
        let _state = StateLock::take(paths)?;
        let mut current = super::load_records(paths)?;
        if attempt < 3 && current.inventory() != before {
            continue;
        }
        commit_authored_records(paths, &mut current, &wire, &minted)?;
        return Ok((minted, current, out));
    }
    unreachable!("bounded loop returns on attempt == 3")
}

/// Network ingress may keep the valid part of an offered stream. Local authoring
/// promises the complete signed operation, including its wraps and resealed
/// values. Check causal retention, allowing already-held records, before any
/// write; a capacity or validation failure discards this in-memory candidate.
pub(super) fn commit_authored_records(
    paths: &Paths,
    current: &mut Store,
    wire: &[u8],
    minted: &[Record],
) -> crate::Result<()> {
    let added = current.ingest(wire)?;
    if minted.iter().any(|record| !current.holds(record.id())) {
        return Err(Error::new(
            "authored membership batch could not be retained completely",
        ));
    }
    if added > 0 {
        fsx::write_atomic(&paths.records_file(), &current.encode(), 0o600)?;
    }
    Ok(())
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
        // Draft authorization reads share one observation. A busy writer yields within
        // the caller's deadline instead of turning its unavailable roster into an error.
        let Some(observation) = StateLock::observe_until(paths, deadline)? else {
            return Ok(None);
        };
        let snapshot = super::load_records(paths)?;
        let before = snapshot.inventory();
        let (minted, out) = draft(&snapshot)?;
        drop(observation);
        if minted.is_empty() {
            return Ok(Some((minted, snapshot, out)));
        }
        let wire = encode_records(&minted);
        #[cfg(test)]
        author_batch_tests::before_commit();
        let Some(_state) = StateLock::try_take_until(paths, deadline)? else {
            return Ok(None);
        };
        let mut current = super::load_records(paths)?;
        if attempt < 3 && current.inventory() != before {
            continue;
        }
        commit_authored_records(paths, &mut current, &wire, &minted)?;
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
            && let Some(pin) = &cfg.signer
            && let Ok(Some(bytes)) = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
            && let Ok((env, _, _)) =
                envelope::verify_for_reader(bytes.as_slice(), &cfg.name, pin, &cfg.followed, None)
        {
            // A current signer can carry a broken older prefix before the
            // trusted pin. Only verified predecessors belong to its lineage.
            lineage_ok = signer == &env.signer || env.reached_from().contains(signer);
        }
        if cfg.signer.is_some() && !lineage_ok {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod lineage_tests {
    use super::*;

    const PIN: [u8; 32] = [53; 32];
    const NEXT: [u8; 32] = [67; 32];
    const UNRELATED: [u8; 32] = [71; 32];
    const VAULT: &str = "lineage-test";

    fn wire(name: &str, seeds: &[[u8; 32]]) -> Vec<u8> {
        signed(
            name,
            seeds.last().unwrap(),
            envelope::test_chain(name, seeds),
        )
    }

    fn signed(name: &str, seed: &[u8; 32], handoff: Vec<([u8; 32], [u8; 64])>) -> Vec<u8> {
        envelope::sign_unchecked(
            &envelope::Unsigned {
                name: name.into(),
                seq: 1,
                time: 1,
                handoff,
                parents: Vec::new(),
                lockers: vec![envelope::Locker {
                    label: "admin".into(),
                    armored: envelope::test_armor(b"inert lineage fixture"),
                }],
            },
            seed,
        )
        .unwrap()
    }

    fn configured() -> (aterm_tempfile::TempDir, Paths) {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        super::super::save_config(
            &paths,
            &Config {
                source: Source::Path(home.path().join("unused.secrets")),
                name: VAULT.into(),
                signer: Some(envelope::signer_public(&PIN).unwrap()),
                followed: Vec::new(),
                host: "m1".into(),
            },
        )
        .unwrap();
        (home, paths)
    }

    #[test]
    fn genesis_lineage_accepts_an_endorsed_rotation_before_the_pin_is_updated() {
        let (_home, paths) = configured();
        let pin = envelope::signer_public(&PIN).unwrap();
        assert!(genesis_signer_matches_local_lineage(&paths, &pin).unwrap());
        let bytes = wire(VAULT, &[PIN, NEXT]);
        fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
        assert!(
            genesis_signer_matches_local_lineage(&paths, &envelope::signer_public(&NEXT).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn genesis_lineage_refuses_unverified_or_unrelated_cached_claims() {
        let valid = wire(VAULT, &[PIN, NEXT]);
        let damaged = String::from_utf8(valid.clone())
            .unwrap()
            .replace("seq: 1\n", "seq: 2\n")
            .into_bytes();
        assert_ne!(valid, damaged, "the negative fixture changed signed bytes");
        let cases = [
            ("unrelated signer", wire(VAULT, &[NEXT])),
            ("other vault", wire("other-vault", &[PIN, NEXT])),
            ("damaged signature", damaged),
            ("unrelated handoff", wire(VAULT, &[UNRELATED, NEXT])),
        ];
        for (why, bytes) in cases {
            let (_home, paths) = configured();
            fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
            assert!(
                !genesis_signer_matches_local_lineage(
                    &paths,
                    &envelope::signer_public(&NEXT).unwrap()
                )
                .unwrap(),
                "accepted {why}"
            );
        }
    }

    #[test]
    fn genesis_lineage_does_not_admit_a_broken_prefix_before_the_trusted_pin() {
        let (_home, paths) = configured();
        let mut handoff = envelope::test_chain(VAULT, &[UNRELATED, PIN, NEXT]);
        handoff[0].1[0] ^= 1;
        let bytes = signed(VAULT, &NEXT, handoff);
        fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
        assert!(
            genesis_signer_matches_local_lineage(&paths, &envelope::signer_public(&NEXT).unwrap())
                .unwrap(),
            "the valid suffix from the pinned key remains usable"
        );
        assert!(
            !genesis_signer_matches_local_lineage(
                &paths,
                &envelope::signer_public(&UNRELATED).unwrap()
            )
            .unwrap(),
            "a forged predecessor is not part of the verified suffix"
        );
    }
}

pub(super) fn post_records_best_effort(paths: &Paths, wire: &[u8]) {
    if let Ok(Some(cfg)) = super::config(paths)
        && let Source::Url(url) = &cfg.source
    {
        let _ = transport::post_records(url, wire);
    }
}

pub(super) struct AuthorCtx<'a> {
    pub(super) actor: &'a MemberId,
    pub(super) writer_seed: &'a [u8; 32],
    pub(super) incarnation: [u8; 16],
    pub(super) authored_ms: u64,
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
    mut sign_proof: impl FnMut(&RecordId, &MemberId) -> [u8; 64],
) -> crate::Result<Option<Record>> {
    if node_key == [0; 32] {
        return Ok(None);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let Some(observation) = StateLock::observe_until(paths, deadline)? else {
        return Ok(None);
    };
    let store = load_records(paths)?;
    let Some(log) = store.log() else {
        return Ok(None);
    };
    let Some((actor, _, _)) = open_local_member_signer_held(paths, log, &observation)? else {
        return Ok(None);
    };
    if log.membership().get(&actor).and_then(|m| m.node_key) == Some(node_key) {
        return Ok(None);
    }
    drop(observation);
    let Some((minted, _, ())) = author_and_commit_records_until(paths, deadline, |store| {
        let Some(mut log) = store.log().cloned() else {
            return Ok((Vec::new(), ()));
        };
        let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
            return Ok((Vec::new(), ()));
        };
        if log.membership().get(&actor).and_then(|m| m.node_key) == Some(node_key) {
            return Ok((Vec::new(), ()));
        }
        let proof = sign_proof(log.network(), &actor);
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
    })?
    else {
        return Ok(None);
    };
    if !minted.is_empty() {
        post_records_aside(paths, encode_records(&minted));
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
    if membership.removal(&entrant_actor).is_some()
        || log.recipient_is_removed(&entrant_recipient)
        || (entrant.node_key != [0; 32]
            && matches!(
                membership.nodes().get(&entrant.node_key),
                Some(crate::membership_log::NodeStanding::Removed(_))
            ))
    {
        return Err(Error::new(
            "the vault removed this Mac, and an Admit does not let a removed Mac back in",
        ));
    }
    if membership.members().iter().any(|(m_actor, m)| {
        (*m_actor == entrant_actor
            || m.binding.recipient() == entrant_recipient
            || (entrant.node_key != [0; 32] && m.node_key == Some(entrant.node_key)))
            && log.left_out(&opened, &EpochAudience::Everyone, m_actor)
    }) || log.audience_left_out_recipient(&EpochAudience::Everyone, &entrant_recipient)
    {
        return Err(Error::new(
            "the vault's newest key leaves this Mac out on purpose, and an Admit does not let it back in",
        ));
    }
    let local_roster = local_swarm_roster_for_log(paths, &log)?;
    let opened_roster = roster_from_doc_or_opened(None, Some(&opened), Some(&log));
    let node_hex = aterm_codec::hex::encode_lower(&entrant.node_key);
    let writer_hex = aterm_codec::hex::encode_lower(entrant.request.binding().writer());
    let incarnation_hex = aterm_codec::hex::encode_lower(entrant.request.binding().incarnation());
    let machine_hex = aterm_codec::hex::encode_lower(entrant.request.binding().machine());
    let denied_by_roster = [local_roster.as_ref(), opened_roster.as_ref()]
        .into_iter()
        .flatten()
        .any(|roster| {
            roster.is_label_denied(&entrant.label)
                || roster.is_label_denied(&entrant.name)
                || (entrant.node_key != [0; 32] && roster.is_node_denied(&node_hex))
                || roster.is_node_denied(&writer_hex)
                || roster.is_node_denied(&incarnation_hex)
                || roster.is_node_denied(&machine_hex)
                || recipient_denied_by_roster_or_log(Some(roster), &log, &entrant_recipient)
        });
    if denied_by_roster || recipient_denied_by_roster_or_log(None, &log, &entrant_recipient) {
        return Err(Error::new(
            "this Mac is on the swarm deny-list, and an Admit does not let a denied Mac back in",
        ));
    }
    if membership.members().iter().any(|(m_actor, m)| {
        (*m_actor == entrant_actor
            || m.binding.recipient() == entrant_recipient
            || (entrant.node_key != [0; 32] && m.node_key == Some(entrant.node_key)))
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

/// Sign [`Body::Remove`] for `target` with this member's Commons device key and mint a new epoch
/// for every audience it was in that this Mac opens (`[everyone]` with the revoke cascade,
/// Commons, and each host list whose newest epoch wraps it), wrapped to the members who remain
/// (`F16`, `§3.3.7`): [`super::remove_member`] by actor, posted to the vault's source. Where
/// another member's removal of `target` is here already, it signs this Mac's share at that
/// removal's cutoff instead, so the two form a quorum `Remove`, and a repeat signs nothing.
///
/// # Errors
/// Local custody holds no seeded log, this Mac is not an active bound member, or `target` has no
/// entry in the log.
pub fn remove_member_in_log(paths: &Paths, target: MemberId) -> crate::Result<Vec<Record>> {
    let minted = super::remove::remove_actor(paths, target)?;
    if !minted.is_empty() {
        let wire = encode_records(&minted);
        post_records_best_effort(paths, &wire);
    }
    Ok(minted)
}

#[cfg(test)]
pub(super) fn retire_recipients_in_log_local(
    paths: &Paths,
    lock: &StateLock,
    active_targets: &[MemberId],
    exclude_recipients: &BTreeSet<Recipient>,
) -> crate::Result<Vec<Record>> {
    let store = super::load_records(paths)?;
    let minted = draft_retire_in_log_with(
        paths,
        &store,
        active_targets,
        exclude_recipients,
        Some(lock),
    )?;
    if !minted.is_empty() {
        let wire = encode_records(&minted);
        let _ = super::save_records_unlocked(paths, &wire)?;
        aterm_durable::durable_dir(&paths.dir).map_err(|e| Error::new(e.to_string()))?;
        post_records_best_effort(paths, &wire);
    }
    Ok(minted)
}

pub(super) fn draft_retire_in_log(
    paths: &Paths,
    store: &Store,
    targets: &[MemberId],
    exclude_recipients: &BTreeSet<Recipient>,
) -> crate::Result<Vec<Record>> {
    draft_retire_in_log_with(paths, store, targets, exclude_recipients, None)
}

pub(super) fn draft_retire_in_log_with(
    paths: &Paths,
    store: &Store,
    targets: &[MemberId],
    exclude_recipients: &BTreeSet<Recipient>,
    lock: Option<&StateLock>,
) -> crate::Result<Vec<Record>> {
    let Some(mut log) = store.log().cloned() else {
        return Err(Error::new("this machine holds no membership log"));
    };
    if !log.has_commons_epochs() {
        return Err(Error::new(
            "this membership log has no Commons control epochs",
        ));
    }
    let signer = match lock {
        Some(lock) => open_local_member_signer_held(paths, &log, lock),
        None => open_local_member_signer(paths, &log),
    }?;
    let Some((actor, device, incarnation)) = signer else {
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

pub(super) fn carried_audience_recipients(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    audience: &EpochAudience,
    active_members: &BTreeSet<Recipient>,
    exclude_recipients: &BTreeSet<Recipient>,
) -> BTreeSet<Recipient> {
    let removed = log.removed_recipients();
    let roster = roster_from_doc_or_opened(None, Some(opened), Some(log));
    let membership = log.membership();
    let mut has_active_seat: BTreeSet<Recipient> = BTreeSet::new();
    let mut has_left_out_seat: BTreeSet<Recipient> = BTreeSet::new();
    for (actor, m) in membership.members() {
        let rec = m.binding.recipient();
        if log.left_out(opened, audience, actor)
            || log.left_out(opened, &EpochAudience::Everyone, actor)
            || roster
                .as_ref()
                .is_some_and(|r| member_denied_by_roster(r, m))
        {
            has_left_out_seat.insert(rec);
        } else {
            has_active_seat.insert(rec);
        }
    }
    let left_out_recipients: BTreeSet<Recipient> = has_left_out_seat
        .into_iter()
        .filter(|rec| !has_active_seat.contains(rec))
        .collect();
    let recipient_denied_by_roster =
        |r: &Recipient| recipient_denied_by_roster_or_log(roster.as_ref(), log, r);
    let mut out: BTreeSet<Recipient> = active_members
        .iter()
        .copied()
        .filter(|r| {
            !removed.contains(r)
                && !exclude_recipients.contains(r)
                && !left_out_recipients.contains(r)
                && !recipient_denied_by_roster(r)
                && !log.audience_left_out_recipient(audience, r)
                && !log.audience_left_out_recipient(&EpochAudience::Everyone, r)
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
                && !recipient_denied_by_roster(&r)
                && !log.audience_left_out_recipient(audience, &r)
                && !log.audience_left_out_recipient(&EpochAudience::Everyone, &r)
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
    rotate_after_removal(
        log,
        opened,
        author,
        exclude_recipients,
        &Rotation::default(),
        minted,
    )
    .map(|_| ())
}

/// What [`rotate_after_removal`] reseals beyond `[everyone]` and Commons.
#[derive(Default)]
pub(super) struct Rotation<'a> {
    /// The `[everyone]` entries to put under the new epoch, where they differ from the opened
    /// ones (a member's removal writes its cascade there, F16).
    pub(super) everyone: Option<&'a BTreeMap<String, Entry>>,
    /// Also rotate each host list this Mac opens whose newest epoch wraps a Mac the log removed
    /// (F16: a removal mints a new epoch for every audience the Mac was in, §3.3.7).
    pub(super) hosts: bool,
}

/// Mint a new epoch for `[everyone]` (where the log has one), for Commons (where the log has
/// Commons epochs) and, with `rotation.hosts`, for each host list this Mac opens whose newest
/// epoch wraps a removed Mac; wrap each to the members who remain in it and reseal its current
/// entries under it (§3.3.7). An audience this Mac does not open whole (an uncovered keyed epoch it
/// lacks, F21 rule 7, or another side's newest epoch) gets only what `rotation` changed there.
/// Returns the audiences it rotated, as `everyone`, `commons` or the host list's header.
///
/// # Errors
/// A record fails to draft, sign or insert.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "KeyedEpochs",
        action = "BlindReput",
        project = "aterm_secrets::client::remove::tests::keyed_epochs_transition"
    )
)]
pub(super) fn rotate_after_removal(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    author: &AuthorCtx<'_>,
    exclude_recipients: &BTreeSet<Recipient>,
    rotation: &Rotation<'_>,
    minted: &mut Vec<Record>,
) -> crate::Result<Vec<String>> {
    let mut rotated = Vec::new();
    let membership = log.membership();
    let roster = roster_from_doc_or_opened(None, Some(opened), Some(log));
    let remaining_members: BTreeSet<Recipient> = membership
        .members()
        .values()
        .filter(|m| {
            !log.recipient_is_removed(&m.binding.recipient())
                && !membership.member_node_removed(m)
                && !roster
                    .as_ref()
                    .is_some_and(|r| member_denied_by_roster(r, m))
        })
        .map(|m| m.binding.recipient())
        .filter(|r| !exclude_recipients.contains(r))
        .collect();
    if remaining_members.is_empty() {
        return Ok(rotated);
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
        // Every entry is resealed only where this Mac opens all of `[everyone]` and every keyed
        // epoch there; else only what `rotation.everyone` changed (F21 rule 7, `reput`).
        let view = opened.by_audience.get(&Audience::Everyone);
        if let Some(next) = rotation.everyone.or(view) {
            let whole = opened.reputs_whole(&EpochAudience::Everyone);
            let (puts, dropped) = reput(whole, view, next);
            for entry in puts {
                let put_rec = log
                    .draft_put(author.actor, author.authored_ms, ep_id, &ep_key, entry)?
                    .sign(author.writer_seed, author.incarnation)?;
                let _ = log.insert(put_rec.clone())?;
                minted.push(put_rec);
            }
            for name in dropped {
                let del_rec = log
                    .draft_delete(author.actor, author.authored_ms, ep_id, &ep_key, name)?
                    .sign(author.writer_seed, author.incarnation)?;
                let _ = log.insert(del_rec.clone())?;
                minted.push(del_rec);
            }
        }
        rotated.push("everyone".to_owned());
    }

    if rotation.hosts {
        rotated.extend(rotate_host_lists(
            log,
            opened,
            author,
            exclude_recipients,
            minted,
        )?);
    }

    if !opened.max_generation.contains_key(&EpochAudience::Commons) {
        return Ok(rotated);
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
    // The descriptor this Mac reads is resealed only where it opens all of Commons (rule 7).
    if let Some(ctrl) = &opened.commons_control
        && opened
            .fully_unwrapped_audiences
            .contains(&EpochAudience::Commons)
    {
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
    rotated.push("commons".to_owned());
    Ok(rotated)
}

/// Mint the next epoch of every host list this Mac opens whose newest epoch is still wrapped to a
/// Mac the log removed, or that has two or more newest epochs (the coverage rule, §3.3.7), or that
/// this Mac reseals under F21's rule 5 ([`reseal_ready`]): wrapped to the recipients it carries
/// over and resealing the list's current entries ([`reseal_audience`]). Returns each rotated list's
/// header. A list this Mac cannot open is left to its own members
/// ([`super::tend_epochs`]).
///
/// # Errors
/// A record fails to draft, sign or insert.
pub(super) fn rotate_host_lists(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    author: &AuthorCtx<'_>,
    exclude_recipients: &BTreeSet<Recipient>,
    minted: &mut Vec<Record>,
) -> crate::Result<Vec<String>> {
    let mut rotated = Vec::new();
    let removed = log.removed_recipients();
    let lists: Vec<(Vec<String>, Vec<RecordId>)> = opened
        .maximal_epochs
        .iter()
        .filter_map(|(audience, max_ids)| match audience {
            EpochAudience::Hosts(hosts) => Some((hosts.clone(), max_ids.clone())),
            _ => None,
        })
        .collect();
    for (hosts, max_ids) in lists {
        let opens_all = max_ids
            .iter()
            .all(|id| opened.unwrapped_keys.contains_key(id));
        let wraps_removed = max_ids.iter().any(|id| {
            opened
                .wrapped_recipients
                .get(id)
                .is_some_and(|set| !set.is_disjoint(&removed))
        });
        let audience = EpochAudience::Hosts(hosts.clone());
        if !opens_all || !(wraps_removed || max_ids.len() > 1 || reseal_ready(opened, &audience)) {
            continue;
        }
        let recipients = carried_audience_recipients(
            log,
            opened,
            &audience,
            &BTreeSet::new(),
            exclude_recipients,
        );
        reseal_audience(log, opened, author, &audience, &recipients, minted)?;
        rotated.push(Audience::Hosts(hosts).header());
    }
    Ok(rotated)
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
    let local_roster = local_swarm_roster_for_log(paths, &log)?;
    let active_recipients: BTreeSet<Recipient> = log
        .membership()
        .members()
        .iter()
        .filter(|(m_actor, m)| {
            !log.recipient_is_removed(&m.binding.recipient())
                && !log.membership().member_node_removed(m)
                && !log.audience_left_out(&EpochAudience::Everyone, m_actor)
                && !log
                    .audience_left_out_recipient(&EpochAudience::Everyone, &m.binding.recipient())
                && !local_roster
                    .as_ref()
                    .is_some_and(|r| member_denied_by_roster(r, m))
        })
        .map(|(_, m)| m.binding.recipient())
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
    let opened_roster = roster_from_doc_or_opened(None, Some(&opened), Some(&log));
    let roster_denies = |m: &crate::membership_log::Member| {
        local_roster
            .as_ref()
            .is_some_and(|r| member_denied_by_roster(r, m))
            || opened_roster
                .as_ref()
                .is_some_and(|r| member_denied_by_roster(r, m))
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
                false,
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
        let active_member = |m: &crate::membership_log::Member| {
            !log.recipient_is_removed(&m.binding.recipient())
                && !membership.member_node_removed(m)
                && !log
                    .audience_left_out_recipient(&EpochAudience::Everyone, &m.binding.recipient())
                && !roster_denies(m)
        };
        let commons_recipients: BTreeSet<Recipient> = membership
            .members()
            .iter()
            .filter(|(m_actor, m)| {
                active_member(m)
                    && !log.left_out(&opened, &EpochAudience::Commons, m_actor)
                    && !log.left_out(&opened, &EpochAudience::Everyone, m_actor)
                    && !log.audience_left_out_recipient(
                        &EpochAudience::Commons,
                        &m.binding.recipient(),
                    )
            })
            .map(|(_, m)| m.binding.recipient())
            .collect();
        let everyone_recipients: BTreeSet<Recipient> = membership
            .members()
            .iter()
            .filter(|(m_actor, m)| {
                active_member(m) && !log.left_out(&opened, &EpochAudience::Everyone, m_actor)
            })
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

/// The text [`Audience`] an epoch of `audience` seals entries for; `None` for Commons, whose one
/// value is the control descriptor.
fn text_audience(audience: &EpochAudience) -> Option<Audience> {
    match audience {
        EpochAudience::Everyone => Some(Audience::Everyone),
        EpochAudience::Hosts(hosts) => Some(Audience::Hosts(hosts.clone())),
        EpochAudience::Commons => None,
    }
}

/// Whether this Mac reseals `audience` under F21's rule 5: the audience has an uncovered keyed
/// epoch a current member holds, and this Mac opens every one of them and every newest epoch of
/// the audience, so its counting view holds every winner there.
pub(super) fn reseal_ready(
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    audience: &EpochAudience,
) -> bool {
    opened
        .uncovered_keyed
        .get(audience)
        .is_some_and(|epochs| !epochs.is_empty())
        && !opened.awaits_reseal(audience)
        && opened.fully_unwrapped_audiences.contains(audience)
}

/// Mint `audience`'s next epoch, wrapped to `recipients`, serving both §3.3.7's coverage rule and
/// F21's reseal (rule 5) in one pass:
/// - first, where this Mac opens the whole audience, its attestation of each uncovered keyed epoch
///   of `audience` it opens: a [`Body::Wrap`] of it to this Mac itself, which [`Log::draft_wrap`]
///   checks against the epoch's commitment, and which rule 4 reads as this Mac having that key. A
///   Mac that does not open it whole (one a host list names but `[everyone]` left out) carries no
///   values, so it attests nothing and its epoch covers nothing;
/// - one [`Body::Epoch`] at `max_generation + 1` with every dependent of those epochs in its past
///   ([`Log::draft_epoch_following`]);
/// - where this Mac opens the whole audience, a re-put of each winner from the counting view
///   (`opened.by_audience`, or Commons' control descriptor), never from a document, which may
///   still hold a removed Mac's split-time values. Where it attests nothing, only where it also
///   opens every keyed epoch there ([`OpenedAudienceEntries::reputs_whole`]); where it attests
///   but lacks a covered keyed epoch's key, only the winners whose op sits under an epoch it
///   attests, the values its reseal carries;
/// - a [`Body::Delete`] of each name whose winner is a `Delete` under a keyed epoch.
///
/// # Errors
/// A record fails to draft, sign or insert.
pub(super) fn reseal_audience(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    author: &AuthorCtx<'_>,
    audience: &EpochAudience,
    recipients: &BTreeSet<Recipient>,
    minted: &mut Vec<Record>,
) -> crate::Result<()> {
    let full = opened.fully_unwrapped_audiences.contains(audience);
    let attest: Vec<RecordId> = opened
        .uncovered_keyed
        .get(audience)
        .into_iter()
        .flatten()
        .filter(|epoch| full && opened.keyed_keys.contains_key(*epoch))
        .copied()
        .collect();
    let follow: Vec<RecordId> = if attest.is_empty() {
        Vec::new()
    } else {
        log.keyed_epochs()
            .into_iter()
            .filter(|keyed| attest.contains(&keyed.epoch))
            .flat_map(|keyed| keyed.dependents)
            .collect()
    };
    let me = log
        .membership()
        .get(author.actor)
        .map(|m| m.binding.recipient());
    if let Some(me) = me {
        for epoch in &attest {
            let wrap_rec = log
                .draft_wrap(
                    author.actor,
                    author.authored_ms,
                    *epoch,
                    &opened.keyed_keys[epoch],
                    me,
                )?
                .sign(author.writer_seed, author.incarnation)?;
            let _ = log.insert(wrap_rec.clone())?;
            minted.push(wrap_rec);
        }
    }
    let next_gen = opened
        .max_generation
        .get(audience)
        .copied()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let (ep_draft, ep_key) = log.draft_epoch_following(
        author.actor,
        author.authored_ms,
        audience.clone(),
        next_gen,
        &follow,
    )?;
    let ep_rec = ep_draft.sign(author.writer_seed, author.incarnation)?;
    let ep_id = *ep_rec.id();
    let _ = log.insert(ep_rec.clone())?;
    minted.push(ep_rec);
    for &recipient in recipients {
        let wrap_rec = log
            .draft_wrap(author.actor, author.authored_ms, ep_id, &ep_key, recipient)?
            .sign(author.writer_seed, author.incarnation)?;
        let _ = log.insert(wrap_rec.clone())?;
        minted.push(wrap_rec);
    }
    let whole = if attest.is_empty() {
        opened.reputs_whole(audience)
    } else {
        full
    };
    // A resealer without some covered keyed epoch's key carries only what its reseal covers.
    let carried = |text: &Audience, name: &String| {
        !opened.keyless_keyed.contains(audience)
            || opened
                .winning_ops
                .get(&(text.clone(), name.clone()))
                .and_then(|op| log.get(op))
                .is_some_and(|record| match record.body() {
                    Body::Put { epoch, .. } => attest.contains(epoch),
                    _ => false,
                })
    };
    let Some(text) = text_audience(audience) else {
        if let Some(ctrl) = &opened.commons_control
            && whole
        {
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
        return Ok(());
    };
    if whole && let Some(entries) = opened.by_audience.get(&text) {
        let entries: Vec<&Entry> = entries
            .iter()
            .filter(|(name, _)| carried(&text, name))
            .map(|(_, entry)| entry)
            .collect();
        for entry in entries {
            let put_rec = log
                .draft_put(author.actor, author.authored_ms, ep_id, &ep_key, entry)?
                .sign(author.writer_seed, author.incarnation)?;
            let _ = log.insert(put_rec.clone())?;
            minted.push(put_rec);
        }
    }
    for name in opened.keyed_deletes.get(&text).into_iter().flatten() {
        let del_rec = log
            .draft_delete(author.actor, author.authored_ms, ep_id, &ep_key, name)?
            .sign(author.writer_seed, author.incarnation)?;
        let _ = log.insert(del_rec.clone())?;
        minted.push(del_rec);
    }
    Ok(())
}

/// The coverage rule of §3.3.7 for `[everyone]` and Commons: where an audience has two or more
/// newest epochs, or a newest epoch still wrapped to a Mac the log removed, mint one higher epoch
/// that covers them, wrapped to the recipients it carries over, only when this Mac opens every
/// newest epoch. An unobserved wrap does not prove that an unopened epoch has no holder.
/// Its entries are resealed under it
/// only where this Mac opens the whole audience, and an audience with an uncovered keyed epoch this
/// Mac does not open is left to a holder's reseal (F21 rule 7). With `reseal` (the vault pass's
/// ranked tender, `tend_epochs_at`), where this Mac holds every uncovered keyed epoch of an
/// audience and every newest epoch there, it reseals the audience ([`reseal_audience`], F21 rule
/// 5), and one epoch serves both rules. Without it (the Commons repair, which runs unranked on
/// every exchange), an audience with an uncovered keyed epoch is left to the tender, so Macs do
/// not reseal at once at a heal and the resealer records rule 8.
///
/// # Errors
/// A record fails to draft, sign or insert.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "KeyedEpochs",
        action = "Reseal",
        project = "aterm_secrets::client::remove::tests::keyed_epochs_transition"
    )
)]
pub(super) fn coalesce_concurrent_commons_epochs(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    author: &AuthorCtx<'_>,
    active_recipients: &BTreeSet<Recipient>,
    minted: &mut Vec<Record>,
    reseal: bool,
) -> crate::Result<()> {
    // Without `reseal`, an audience with an uncovered keyed epoch waits for the ranked tender.
    let tender_only = |audience: &EpochAudience| {
        !reseal
            && opened
                .uncovered_keyed
                .get(audience)
                .is_some_and(|epochs| !epochs.is_empty())
    };
    let removed = log.removed_recipients();
    let max_everyone = opened
        .maximal_epochs
        .get(&EpochAudience::Everyone)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    // An absent wrap may still be in flight. Neither an empty recipient set nor one
    // containing no currently active member proves that an unopened epoch may be replaced.
    let can_open_all_max_everyone = !max_everyone.is_empty()
        && max_everyone
            .iter()
            .all(|ep_id| opened.unwrapped_keys.contains_key(ep_id));
    let everyone_wraps_removed = !removed.is_empty()
        && max_everyone.iter().any(|ep_id| {
            opened
                .wrapped_recipients
                .get(ep_id)
                .is_some_and(|set| !set.is_disjoint(&removed))
        });
    let empty_exclude = BTreeSet::new();
    // F21 rule 7: an audience with an uncovered keyed epoch this Mac does not open waits for a
    // holder's reseal, and only an audience it opens whole is resealed entry by entry; rule 5:
    // a holder of every uncovered keyed epoch reseals it.
    if !tender_only(&EpochAudience::Everyone)
        && (reseal_ready(opened, &EpochAudience::Everyone)
            || (can_open_all_max_everyone
                && !opened.awaits_reseal(&EpochAudience::Everyone)
                && (max_everyone.len() > 1 || everyone_wraps_removed)))
    {
        let everyone_recipients = carried_audience_recipients(
            log,
            opened,
            &EpochAudience::Everyone,
            active_recipients,
            &empty_exclude,
        );
        reseal_audience(
            log,
            opened,
            author,
            &EpochAudience::Everyone,
            &everyone_recipients,
            minted,
        )?;
    }

    let max_commons = opened
        .maximal_epochs
        .get(&EpochAudience::Commons)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let can_open_all_max_commons = !max_commons.is_empty()
        && max_commons
            .iter()
            .all(|ep_id| opened.unwrapped_keys.contains_key(ep_id));
    let commons_wraps_removed = !removed.is_empty()
        && max_commons.iter().any(|ep_id| {
            opened
                .wrapped_recipients
                .get(ep_id)
                .is_some_and(|set| !set.is_disjoint(&removed))
        });
    if tender_only(&EpochAudience::Commons)
        || (!reseal_ready(opened, &EpochAudience::Commons)
            && (!can_open_all_max_commons
                || opened.awaits_reseal(&EpochAudience::Commons)
                || (max_commons.len() <= 1 && !commons_wraps_removed)))
    {
        return Ok(());
    }
    let commons_recipients = carried_audience_recipients(
        log,
        opened,
        &EpochAudience::Commons,
        active_recipients,
        &empty_exclude,
    );
    reseal_audience(
        log,
        opened,
        author,
        &EpochAudience::Commons,
        &commons_recipients,
        minted,
    )
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

fn is_enrollment_denied_by_roster(
    paths: &Paths,
    log: &Log,
    binding: &crate::commons::binding::Binding,
    labels: &[&str],
    node_key: Option<&[u8; 32]>,
) -> crate::Result<bool> {
    let Some(roster) = local_swarm_roster_for_log(paths, log)? else {
        return Ok(false);
    };
    if roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.writer()))
        || roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.incarnation()))
        || roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.machine()))
    {
        return Ok(true);
    }
    if let Some(key) = node_key
        && *key != [0; 32]
        && roster.is_node_denied(&aterm_codec::hex::encode_lower(key))
    {
        return Ok(true);
    }
    let rec_str = binding.recipient().to_bech32();
    if let Some(row) = roster.member_by_recipient(&rec_str) {
        return Ok(super::steward::member_is_denied_in_roster(&roster, row));
    }
    if labels.iter().any(|l| roster.is_label_denied(l)) {
        return Ok(true);
    }
    Ok(labels.iter().any(|l| {
        roster
            .member_by_label_or_alias(l)
            .is_some_and(|row| super::steward::member_is_denied_in_roster(&roster, row))
    }))
}

fn plan_log_enrollment(
    paths: &Paths,
    log: &Log,
    trust: crate::commons::journal::Trust,
    binding: &crate::commons::binding::Binding,
    already_locked: bool,
) -> crate::Result<LogEnrollPlan> {
    if *binding.network() != trust.network || *binding.authority() != trust.authority {
        return Err(Error::new("Commons enrollment has another local root"));
    }
    let membership = log.membership();
    if membership.removal(&binding.actor()).is_some()
        || log.recipient_is_removed(&binding.recipient())
        || matches!(
            membership.node(binding.writer()),
            crate::membership_log::NodeStanding::Removed(_)
        )
    {
        return Err(Error::new("Commons enrollment recipient was removed"));
    }
    if let Some((actor, existing)) = membership
        .members()
        .iter()
        .filter(|(_, m)| m.binding.recipient() == binding.recipient())
        .max_by_key(|(actor, m)| (log.record_index(&m.entry).unwrap_or(0), *actor))
    {
        if &existing.binding != binding {
            return Err(Error::new(
                "Commons enrollment cannot replace an existing device",
            ));
        }
        if log.audience_left_out(&EpochAudience::Commons, actor)
            || log.audience_left_out(&EpochAudience::Everyone, actor)
            || log.audience_left_out_recipient(&EpochAudience::Commons, &binding.recipient())
            || log.audience_left_out_recipient(&EpochAudience::Everyone, &binding.recipient())
            || membership.member_node_removed(existing)
            || is_enrollment_denied_by_roster(
                paths,
                log,
                binding,
                &[&existing.label, &existing.name],
                existing.node_key.as_ref(),
            )?
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
            || log.audience_left_out_recipient(&EpochAudience::Commons, &founder.recipient)
            || log.audience_left_out_recipient(&EpochAudience::Everyone, &founder.recipient)
            || founder.node_key.is_some_and(|nk| {
                nk != [0; 32]
                    && matches!(
                        membership.node(&nk),
                        crate::membership_log::NodeStanding::Removed(_)
                    )
            })
            || is_enrollment_denied_by_roster(
                paths,
                log,
                binding,
                &[&founder.label, &founder.name],
                founder.node_key.as_ref(),
            )?
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
    let cached_admin = if already_locked {
        super::enrollment::admin_cached_unlocked(paths)
    } else {
        super::enrollment::admin_cached_until(paths, Instant::now() + Duration::from_secs(5))
    };
    if let Ok(admin) = cached_admin
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
        if is_enrollment_denied_by_roster(paths, log, binding, &[&machine.name], None)? {
            return Err(Error::new("Commons enrollment recipient is not admitted"));
        }
        let rec_str = binding.recipient().to_bech32();
        let mut combined_roster = local_swarm_roster_for_log(paths, log)?.unwrap_or_default();
        if let Ok(Some(doc_roster)) =
            crate::swarm_roster::SwarmRoster::from_document(&admin.published_doc)
        {
            merge_swarm_roster(&mut combined_roster, doc_roster);
        }
        let own_roster_label = combined_roster
            .member_by_recipient(&rec_str)
            .filter(|row| !super::steward::member_is_denied_in_roster(&combined_roster, row))
            .map(|row| row.label.clone());
        let preferred_label = own_roster_label.as_deref().unwrap_or(&machine.name);
        let mut taken = BTreeSet::new();
        for raw in &combined_roster.deny_labels {
            if let Some((_, canon)) = crate::swarm_roster::leading_m_label(raw) {
                taken.insert(canon);
            }
        }
        for member in &combined_roster.members {
            if member.recipient.trim() == rec_str {
                continue;
            }
            taken.insert(member.label.clone());
            if let Some((_, canon)) = crate::swarm_roster::leading_m_label(&member.label) {
                taken.insert(canon);
            }
            for alias in &member.aliases {
                taken.insert(alias.clone());
                if let Some((_, canon)) = crate::swarm_roster::leading_m_label(alias) {
                    taken.insert(canon);
                }
            }
            if !combined_roster.is_member_denied(member) {
                if let Some(claimed) = member.claimed_label.as_deref().filter(|c| {
                    crate::swarm_roster::leading_m_label(c).is_some_and(|(_, canon)| canon == *c)
                }) {
                    taken.insert(claimed.to_owned());
                }
                if let Some((_, canon)) = crate::swarm_roster::leading_m_label(&member.vault_name) {
                    taken.insert(canon);
                }
            }
        }
        if crate::swarm_roster::leading_m_label(preferred_label).is_none() {
            taken.insert(crate::swarm_roster::DEFAULT_RESERVED_RELEASE_LABEL.to_owned());
            if let Ok(Some(policy)) =
                crate::swarm_roster::SwarmPolicy::from_document(&admin.published_doc)
            {
                taken.extend(
                    policy.effective_reserved(&admin.published_doc, Some(&combined_roster)),
                );
            }
        }
        let label = log
            .next_label(preferred_label, &taken, &BTreeSet::new())
            .ok_or_else(|| Error::new("no m<N> label up to m99999 is free"))?;
        if is_enrollment_denied_by_roster(paths, log, binding, &[&machine.name, &label], None)? {
            return Err(Error::new("Commons enrollment recipient is not admitted"));
        }
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
    let _ = plan_log_enrollment(paths, &log, trust, binding, false)?;
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
    let plan = plan_log_enrollment(paths, &log, trust, &binding, true)?;
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
    let membership = log.membership();
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
            Body::Admit { entrant, .. } if entrant.request.binding().recipient() == recipient => {
                targets.push(*rec.id());
            }
            Body::Node { .. }
                if membership
                    .get(rec.actor())
                    .is_some_and(|m| m.binding.recipient() == recipient) =>
            {
                targets.push(*rec.id());
            }
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

/// How recently a server must have heard a Mac with no device-key proof in the log for the
/// `0.145` mirror to stay on (`DESIGN-fluid-swarm` F20: every member heard in 14 days).
pub(crate) const OLD_MAC_WINDOW_SECS: u64 = transport::HEARD_COVERAGE_SECS;

/// Whether the `0.145` envelope mirror is still needed at `now` (unix seconds), given what the
/// vault's servers heard (`heard`, each subscriber's [`transport::heard_tag`] and last time): one
/// heard within [`OLD_MAC_WINDOW_SECS`] that is no active member's bound recipient in `log`, and
/// no recipient `log` removed, is a Mac that reads only the envelope. Unknown (`None`: no server
/// answered) keeps it on.
pub(crate) fn mirror_still_needed(log: &Log, heard: Option<&transport::Heard>, now: u64) -> bool {
    let Some(heard) = heard else {
        return true;
    };
    if heard
        .anonymous
        .is_some_and(|at| now.saturating_sub(at) <= OLD_MAC_WINDOW_SECS)
    {
        return true;
    }
    let known: BTreeSet<String> = log
        .membership()
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .chain(log.removed_recipients())
        .map(|r| transport::heard_tag(&r))
        .collect();
    heard
        .subscribers
        .iter()
        .any(|(tag, &at)| now.saturating_sub(at) <= OLD_MAC_WINDOW_SECS && !known.contains(tag))
}

/// The union of what every server in `urls` heard ([`transport::fetch_heard`]), each subscriber's
/// latest time in the caller's clock domain; `None` unless a nonempty set of servers all
/// supplied complete coverage.
pub(super) fn heard_across(urls: &[String], now: u64) -> Option<transport::Heard> {
    if urls.is_empty() {
        return None;
    }
    let mut union = transport::Heard {
        as_of: now,
        subscribers: BTreeMap::new(),
        anonymous: None,
    };
    for url in urls {
        let heard = transport::fetch_heard(url).ok()??;
        for (tag, at) in heard.subscribers {
            // A peer's wall clock may differ from ours. Union observation ages rather than
            // letting a slow server clock make a newly heard old Mac appear to have expired.
            let at = now.saturating_sub(heard.as_of.saturating_sub(at));
            let last = union.subscribers.entry(tag).or_insert(0);
            *last = (*last).max(at);
        }
        union.anonymous = union.anonymous.max(
            heard
                .anonymous
                .map(|at| now.saturating_sub(heard.as_of.saturating_sub(at))),
        );
    }
    Some(union)
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
        let _ = super::sync_without_reconcile(paths, extra_urls, None);
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
            let _ = super::fan_out::post_records_to(
                &candidate_urls,
                &store.encode(),
                super::fan_out::one_exchange(),
            );
        }
    }

    let heads = ledger_publish::read_server_heads(paths, extra_urls)?;
    if heads.is_empty() {
        return Ok(None);
    }
    let identity = super::machine_identity(paths)?;
    // This Mac's own last version, where it carries a Mac every server's version lost at a heal
    // (`own_head`): the mirror lets exactly those Macs back in. A carry that fails before anything
    // went out (the merge cannot take a Mac back, the log refuses the draft) is dropped, and the
    // servers' heads are mirrored as they stand.
    let mut tried = MirrorTry::default();
    if let Some(own) = super::own_head::own_head(paths, &config, &identity) {
        match mirror_heads(
            paths,
            extra_urls,
            &config,
            &candidate_urls,
            &heads,
            &identity,
            Some(&own),
            &mut tried,
        ) {
            Err(_) if tried.carried && !tried.put => tried = MirrorTry::default(),
            outcome => return outcome,
        }
    }
    mirror_heads(
        paths,
        extra_urls,
        &config,
        &candidate_urls,
        &heads,
        &identity,
        None,
        &mut tried,
    )
}

/// What one [`mirror_heads`] did: whether it carried a Mac back from this Mac's own last version,
/// and whether it sent an envelope to any server.
#[derive(Default)]
struct MirrorTry {
    carried: bool,
    put: bool,
}

/// [`mirror_log_to_servers`] over the servers' `heads`, read already: merge them, carry back the
/// Macs `own` (this Mac's own last version) holds that they lost (`own_head`), overlay the log,
/// draft what changed into it, and publish the merged version where it differs.
#[expect(
    clippy::too_many_arguments,
    reason = "one mirror pass: the vault, its servers, the heads read and this Mac's own version"
)]
fn mirror_heads(
    paths: &Paths,
    extra_urls: &[String],
    config: &Config,
    candidate_urls: &[String],
    heads: &[ledger_publish::ObservedHead],
    identity: &Identity,
    own: Option<&ledger_publish::ObservedHead>,
    tried: &mut MirrorTry,
) -> crate::Result<Option<u64>> {
    let mut merged = ledger_publish::merge_diverged_heads(heads)?;
    if heads
        .iter()
        .any(|h| super::admin_removed_in_log(paths, config, &h.envelope, identity, &merged.doc))
    {
        return Ok(None);
    }
    let signer_pub = envelope::signer_public(&merged.seed)?;
    let seeded = trusted_seeded_store(paths, config, &signer_pub, &merged.handoff)?;
    let seeded_log = seeded.as_ref().and_then(|(store, _)| store.log());
    // The servers' heads alone are merged, and judged against the log below; from this Mac's own
    // version only the Macs they lost come back, nothing else it held.
    let readmit = match own {
        Some(own) => {
            let lost = super::own_head::judge(paths, own, heads, seeded_log, &identity.recipient());
            if !lost.is_empty() {
                tried.carried = true;
                super::own_head::carry_lost(&mut merged, own, &lost)?;
            }
            lost
        }
        None => BTreeSet::new(),
    };
    let readmitting = (!readmit.is_empty()).then_some(&readmit);
    let mut log_changed_doc = false;
    let mut import = false;
    let mut old_macs_gone = false;
    if let Some(log) = seeded_log {
        if log.removes(&identity.recipient())
            || recipient_denied_by_roster_or_log(None, log, &identity.recipient())
            || log.audience_left_out_recipient(&EpochAudience::Everyone, &identity.recipient())
        {
            return Ok(None);
        }
        // The log speaks for the envelope only where it holds every head's records. A head whose
        // records it lacks waits for them: mirroring then would publish the older values it
        // holds. A head a writer with no seat in the log published is imported as it stands.
        let mut covers = true;
        for head in heads {
            match log_base(Some(log), &head.envelope, identity)? {
                LogBase::Behind => {
                    // A version this Mac published, whose records it held back when the publish
                    // was cut short: taken in, they let this pass mirror (and send them).
                    let adopted = {
                        let _lock = StateLock::take(paths)?;
                        adopt_held_draft(paths, &head.envelope, identity)?
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
            let machines_before: BTreeSet<String> = merged
                .doc
                .machines()
                .into_iter()
                .map(|m| m.recipient)
                .collect();
            log_changed_doc =
                overlay_log_onto_document(log, identity, &mut merged.doc, readmitting)?;
            if log_changed_doc {
                // The overlay wrote the log's roster over the merge. Where the side that dropped
                // an approval wrote it last, it has no row for a Mac carried back, which gets its
                // own row from this Mac's version again, before the merge of heads below would
                // assign it a fresh label.
                if let Some(own) = own
                    && !readmit.is_empty()
                {
                    super::own_head::carry_rows(&mut merged.doc, own, &readmit)?;
                }
                if merged.diverged {
                    ledger_publish::reconcile_merged_metadata_with_log(
                        &mut merged.doc,
                        heads,
                        Some(log),
                        &readmit,
                    )?;
                }
                merged.admissions = merged.admissions.retained(&merged.doc)?;
                // A version that changes who is in always goes out: one that takes a Mac out
                // reseals the vault without that Mac's key, so a removed Mac never keeps reading
                // the last version it held.
                let machines_changed = merged
                    .doc
                    .machines()
                    .into_iter()
                    .map(|m| m.recipient)
                    .collect::<BTreeSet<String>>()
                    != machines_before;
                if !machines_changed {
                    let now = aterm_time::unix_millis().unwrap_or(0) / 1000;
                    let heard = heard_across(candidate_urls, now);
                    old_macs_gone = !mirror_still_needed(log, heard.as_ref(), now);
                }
            }
        } else {
            import = true;
            if overlay_log_commons_onto_document(log, identity, &mut merged.doc, readmitting)? {
                log_changed_doc = true;
                if merged.diverged {
                    ledger_publish::reconcile_merged_metadata_with_log(
                        &mut merged.doc,
                        heads,
                        Some(log),
                        &readmit,
                    )?;
                }
                merged.admissions = merged.admissions.retained(&merged.doc)?;
            }
        }
    }
    // A Mac carried back that every server lists already may still lack a wrap of the newest
    // epochs (`own_head::lost_to`): the draft below writes it, so the pass goes on.
    if !merged.diverged && !log_changed_doc && !import && readmit.is_empty() {
        return Ok(None);
    }
    // F20: once no Mac without a device-key proof in the log was heard within
    // [`OLD_MAC_WINDOW_SECS`], nothing reads a version that only mirrors the log, so none is
    // published. Diverged heads, imported text and a carried Mac still go out: they carry what
    // admins wrote.
    if old_macs_gone && !merged.diverged && !import && readmit.is_empty() {
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
        config,
        &signer_pub,
        &merged.handoff,
        identity,
        &merged.doc,
        Base {
            doc: (!import).then_some(&merged.doc),
            resolve: Some(&ruled),
            readmit: readmitting,
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
        let _ = super::fan_out::post_records_to(
            candidate_urls,
            &encode_records(&drafted.records),
            super::fan_out::one_exchange(),
        );
    }

    let mut urls: Vec<String> = heads
        .iter()
        .map(|h| h.url.clone())
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .collect();
    for u in candidate_urls {
        if !urls.contains(u) {
            urls.push(u.clone());
        }
    }
    // The highest head stands for the merge only where it already holds all of it; one that lacks
    // a Mac carried back does not.
    if merged.diverged
        && readmit.is_empty()
        && !log_changed_doc
        && !needs_stamp
        && drafted.as_ref().is_none_or(|d| d.records.is_empty())
        && ledger_publish::leader_subsumes_heads(heads, &merged)
        && let Some(leader) = heads
            .iter()
            .max_by_key(|h| (h.envelope.seq, h.envelope.digest()))
    {
        tried.put = true;
        if !urls.is_empty() {
            let outcomes =
                ledger_publish::publish_to_servers_for_admin(paths, config, &urls, &leader.raw)?;
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
            let _lock = super::configured_admin(paths, config)?;
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

    tried.put = true;
    if !urls.is_empty() {
        let outcomes = ledger_publish::publish_to_servers_for_admin(paths, config, &urls, &bytes)?;
        if !outcomes.iter().any(|(_, r)| r.is_ok()) {
            let first_err = outcomes
                .into_iter()
                .find_map(|(_, r)| r.err())
                .unwrap_or_else(|| Error::new("no candidate server accepted the mirror publish"));
            return Err(first_err);
        }
    } else if matches!(config.source, Source::Path(_)) {
        let _lock = super::configured_admin(paths, config)?;
        transport::publish(&config.source, &bytes)?;
    }
    // A server took a version this Mac sealed: it is this Mac's own last version now, unless the
    // one kept still carries a Mac this one lacks.
    let log_after = trusted_seeded_store(paths, config, &signer_pub, &merged.handoff)
        .ok()
        .flatten();
    if !readmit.is_empty() {
        super::own_head::note_carried(paths, &readmit);
    }
    super::own_head::keep(
        paths,
        config,
        identity,
        &bytes,
        &merged.doc,
        log_after.as_ref().and_then(|(store, _)| store.log()),
    );
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
        if !at.is_empty()
            && at.bytes().all(|b| b.is_ascii_digit())
            && let Ok(at) = at.parse()
        {
            seen.insert((id, recipient.to_owned()), at);
        }
    }
    if due.is_empty() || !due.bytes().all(|b| b.is_ascii_digit()) {
        return None;
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "KeyedEpochs",
        action = "CloseGap",
        project = "aterm_secrets::client::remove::tests::keyed_epochs_transition"
    )
)]
pub(super) fn close_wrap_gaps(paths: &Paths, now_ms: u64) -> crate::Result<Vec<Record>> {
    if !wrap_gaps_pass(paths, now_ms, None)?.0 {
        return Ok(Vec::new());
    }
    let _author = RecordsAuthorLock::take(paths)?;
    let lock = StateLock::take(paths)?;
    Ok(wrap_gaps_pass(paths, now_ms, Some(&lock))?.1)
}

/// Host for `KeyedEpochs`' environment markers (F21), so `spec_xref_closure` accounts for every
/// model action: the heal and a person's edit are what the Macs around the library do.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "KeyedEpochs",
        action = "Heal",
        reason = "Environment: the split heals and every Mac takes every other's records"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "KeyedEpochs",
        action = "Edit",
        reason = "Environment: a person edits an entry under an epoch every member holds"
    )
)]
#[doc(hidden)]
pub fn keyed_epochs_environment() {}

/// One look at the wrap gaps for [`close_wrap_gaps`]: whether one is due for this member now,
/// and, where `sign` (the caller holds the state lock), the `Wrap`s it signed and kept. Each look
/// writes the gap mark with when this Mac first saw each gap it holds a rank for, so the next
/// pass counts from there, and the next pass with no new record and nothing due stops at the
/// digest.
fn wrap_gaps_pass(
    paths: &Paths,
    now_ms: u64,
    lock: Option<&StateLock>,
) -> crate::Result<(bool, Vec<Record>)> {
    let sign = lock.is_some();
    let nothing = Ok((false, Vec::new()));
    let Some(config) = super::config(paths)? else {
        return nothing;
    };
    if config.signer.is_none() {
        return nothing;
    }
    let mut store = super::load_records(paths)?;
    let Some(log_ref) = store.log() else {
        return nothing;
    };
    if !log_ref.has_epochs() || !commons_log_matches_local_lineage(paths, log_ref)? {
        return nothing;
    }
    let digest = records_heads_digest(log_ref);
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
        match lock {
            Some(lock) => open_local_member_signer_held(paths, &log, lock)?,
            None => open_local_member_signer(paths, &log)?,
        },
        read_identity(paths)?,
    ) else {
        write_gap_mark(paths, &digest, 0, &none_seen)?;
        return nothing;
    };
    let Some(opened) = log.open_audience_entries(&identity)? else {
        write_gap_mark(paths, &digest, 0, &none_seen)?;
        return nothing;
    };
    let local_roster = match lock {
        Some(lock) => local_swarm_roster_for_log_held(paths, &log, lock),
        None => local_swarm_roster_for_log(paths, &log),
    }?;
    let opened_roster = roster_from_doc_or_opened(None, Some(&opened), Some(&log));
    let membership = log.membership();
    let audiences = crate::membership_log::admission_audiences(&opened);
    let mut due_now = Vec::new();
    let mut next_due: Option<u64> = None;
    let mut seen = BTreeMap::new();
    for gap in log.wrap_gaps(&opened, &audiences) {
        if membership.get(&gap.member).is_some_and(|m| {
            local_roster
                .as_ref()
                .is_some_and(|r| member_denied_by_roster(r, m))
                || opened_roster
                    .as_ref()
                    .is_some_and(|r| member_denied_by_roster(r, m))
        }) || recipient_denied_by_roster_or_log(local_roster.as_ref(), &log, &gap.recipient)
            || recipient_denied_by_roster_or_log(opened_roster.as_ref(), &log, &gap.recipient)
        {
            continue;
        }
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
/// this Mac reaches by `posts_by` (one exchange from when it posts where none is given,
/// [`super::admit::post_records_to_servers_by`]): the subscriber's pass after it pulls records.
/// Best effort: a pass that fails here tries again on the next.
pub(super) fn tend_wrap_gaps(paths: &Paths, posts_by: Option<std::time::Instant>) {
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    if close_wrap_gaps(paths, now_ms).is_ok_and(|signed| !signed.is_empty()) {
        let by = posts_by.unwrap_or_else(super::fan_out::one_exchange);
        let _ = super::admit::post_records_to_servers_by(paths, by);
    }
}

#[cfg(test)]
#[path = "log_entries/author_batch_tests.rs"]
mod author_batch_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commons::binding::Request;
    use crate::membership_log::{Entrant, Log, Record};
    use crate::swarm_roster::{SwarmMember, SwarmRoster};

    #[test]
    fn public_roster_and_stamp_distinguish_envelopes_readers_and_parse_failures() {
        let identity = Identity::from_secret_bytes([0x61; 32]);
        let outsider = Identity::from_secret_bytes([0x62; 32]);
        let seal = |value: Vec<u8>| {
            let head = aterm_digest::Sha256::digest(&value);
            let plain = text::render_everyone_locker(&[Entry {
                name: crate::swarm_roster::ROSTER_ENTRY.into(),
                value: SecretBytes::new(value),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            }]);
            let plain = stamped_text(plain.as_slice(), &stamp_line(&[head]).unwrap());
            let cipher = age::encrypt(&[identity.recipient()], plain.as_slice()).unwrap();
            envelope::parse(
                &envelope::sign(
                    &envelope::Unsigned {
                        name: "alab".into(),
                        seq: 1,
                        time: 1_000,
                        handoff: Vec::new(),
                        parents: Vec::new(),
                        lockers: vec![envelope::Locker {
                            label: "1".into(),
                            armored: age::armor(&cipher),
                        }],
                    },
                    &[0x63; 32],
                )
                .unwrap(),
            )
            .unwrap()
        };
        let first = SwarmRoster::default();
        let original = seal(first.to_toml().into_bytes());
        let first_stamp = Stamp::Heads(vec![aterm_digest::Sha256::digest(
            first.to_toml().as_bytes(),
        )]);
        assert_eq!(envelope_stamp(&original, &identity).unwrap(), first_stamp);
        assert_eq!(
            envelope_stamp(&original, &outsider).unwrap(),
            Stamp::NoLocker
        );
        assert_eq!(
            roster_with_log(None, &original, &identity)
                .unwrap()
                .0
                .into_roster()
                .unwrap(),
            Some(first.clone())
        );
        assert!(
            roster_with_log(None, &original, &outsider)
                .unwrap()
                .0
                .into_roster()
                .unwrap()
                .is_none()
        );
        let changed = SwarmRoster {
            deny_labels: vec!["m8".into()],
            ..SwarmRoster::default()
        };
        let next = seal(changed.to_toml().into_bytes());
        let next_stamp = Stamp::Heads(vec![aterm_digest::Sha256::digest(
            changed.to_toml().as_bytes(),
        )]);
        assert_eq!(envelope_stamp(&next, &identity).unwrap(), next_stamp);
        assert_eq!(envelope_stamp(&original, &identity).unwrap(), first_stamp);
        assert_eq!(
            roster_with_log(None, &next, &identity)
                .unwrap()
                .0
                .into_roster()
                .unwrap(),
            Some(changed)
        );
        assert_eq!(
            roster_with_log(None, &original, &identity)
                .unwrap()
                .0
                .into_roster()
                .unwrap(),
            Some(first)
        );
        let malformed = seal(vec![0xff]);
        for _ in 0..2 {
            assert!(
                roster_with_log(None, &malformed, &identity)
                    .unwrap()
                    .0
                    .into_roster()
                    .is_err()
            );
        }
    }

    #[test]
    fn roster_observation_follows_exact_encrypted_history_and_reader() {
        let writer = [0x71; 32];
        let incarnation = [0x72; 16];
        let identity = Identity::from_secret_bytes([0x73; 32]);
        let outsider = Identity::from_secret_bytes([0x74; 32]);
        let request = Request::create(
            [0x75; 16],
            [0x76; 32],
            [0x77; 16],
            incarnation,
            &identity,
            &writer,
        )
        .unwrap();
        let actor = request.binding().actor();
        let founder = Entrant {
            request,
            node_key: [0x78; 32],
            name: "m1.local".into(),
            label: "m1".into(),
        };
        let mut log = Log::new(Record::found(&[0x79; 32], vec![founder], 1_000).unwrap()).unwrap();
        let (draft, epoch_key) = log
            .draft_epoch(&actor, 1_001, EpochAudience::Everyone, 1)
            .unwrap();
        let epoch = draft.sign(&writer, incarnation).unwrap();
        let epoch_id = *epoch.id();
        log.insert(epoch).unwrap();
        let wrap = log
            .draft_wrap(&actor, 1_002, epoch_id, &epoch_key, identity.recipient())
            .unwrap()
            .sign(&writer, incarnation)
            .unwrap();
        log.insert(wrap).unwrap();
        let put = |log: &mut Log, roster: &SwarmRoster, time| {
            let entry = Entry {
                name: crate::swarm_roster::ROSTER_ENTRY.into(),
                value: SecretBytes::new(roster.to_toml().into_bytes()),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            };
            let record = log
                .draft_put(&actor, time, epoch_id, &epoch_key, &entry)
                .unwrap()
                .sign(&writer, incarnation)
                .unwrap();
            log.insert(record).unwrap();
        };
        let first = SwarmRoster::default();
        put(&mut log, &first, 1_003);
        let original = log.clone();
        let empty_envelope = envelope::parse(
            &envelope::sign(
                &envelope::Unsigned {
                    name: "alab".into(),
                    seq: 1,
                    time: 1_000,
                    handoff: Vec::new(),
                    parents: Vec::new(),
                    lockers: Vec::new(),
                },
                &[0x79; 32],
            )
            .unwrap(),
        )
        .unwrap();
        let read = |log: &Log, who: &Identity| {
            roster_with_log(Some((log, records_heads_digest(log))), &empty_envelope, who).unwrap()
        };
        assert_eq!(
            read(&log, &identity).0.into_roster().unwrap(),
            Some(first.clone())
        );
        assert_eq!(read(&log, &identity).1, Some(records_heads_digest(&log)));
        assert_eq!(
            roster_from_log(&log, &identity)
                .unwrap()
                .into_roster()
                .unwrap(),
            Some(first.clone())
        );
        assert!(read(&log, &outsider).0.into_roster().unwrap().is_none());
        assert!(
            roster_from_log(&log, &outsider)
                .unwrap()
                .into_roster()
                .unwrap()
                .is_none()
        );

        let changed = SwarmRoster {
            deny_labels: vec!["m9".into()],
            ..SwarmRoster::default()
        };
        put(&mut log, &changed, 1_004);
        assert_eq!(
            read(&log, &identity).0.into_roster().unwrap(),
            Some(changed.clone())
        );
        assert_eq!(
            roster_from_log(&log, &identity)
                .unwrap()
                .into_roster()
                .unwrap(),
            Some(changed)
        );
        assert_eq!(
            read(&original, &identity).0.into_roster().unwrap(),
            Some(first.clone())
        );

        let (draft, _) = log
            .draft_epoch(&actor, 1_005, EpochAudience::Everyone, 2)
            .unwrap();
        log.insert(draft.sign(&writer, incarnation).unwrap())
            .unwrap();
        let revoked = read(&log, &identity);
        assert!(!revoked.0.visible);
        assert!(revoked.0.into_roster().unwrap().is_none());
        assert_eq!(revoked.1, Some(records_heads_digest(&log)));
        assert_eq!(
            read(&original, &identity).0.into_roster().unwrap(),
            Some(first)
        );
    }

    #[test]
    fn merge_swarm_roster_and_denied_checks_prefer_active_row_over_stale_denied_row() {
        let id1 = Identity::from_secret_bytes([0x11; 32]);
        let id2 = Identity::from_secret_bytes([0x22; 32]);
        let req1 =
            Request::create([2; 16], [3; 32], [1; 16], [1; 16], &id1, &[1; 32]).expect("req1");
        let req2 =
            Request::create([2; 16], [3; 32], [2; 16], [2; 16], &id2, &[2; 32]).expect("req2");
        let rec2_str = id2.recipient().to_bech32();
        let ent1 = Entrant {
            request: req1,
            node_key: [1; 32],
            name: "m1.local".to_owned(),
            label: "m1".to_owned(),
        };
        let ent2 = Entrant {
            request: req2,
            node_key: [2; 32],
            name: "m8.local".to_owned(),
            label: "m8".to_owned(),
        };
        let genesis = Record::found(&[99; 32], vec![ent1, ent2], 1_000).expect("genesis");
        let log = Log::new(genesis).expect("log");

        let stale_denied = SwarmMember {
            recipient: rec2_str.clone(),
            vault_name: "m5".to_owned(),
            label: "m5".to_owned(),
            aliases: vec!["m50".to_owned()],
            hint: None,
            hw_hint: None,
            admitted_ms: 100,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: Some("aa".repeat(16)),
            node_key: Some("aa".repeat(32)),
        };
        let active_row = SwarmMember {
            recipient: rec2_str.clone(),
            vault_name: "m8".to_owned(),
            label: "m8".to_owned(),
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: 200,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: Some("bb".repeat(16)),
            node_key: Some("02".repeat(32)),
        };

        // 1. Both rows in one roster (`m5` denied, `m8` active): `member_denied_by_roster` and
        //    `recipient_denied_by_roster_or_log` must NOT treat `id2` as denied.
        let mut both = SwarmRoster {
            members: vec![stale_denied.clone(), active_row.clone()],
            deny_labels: vec!["m5".to_owned()],
            deny_nodes: vec!["aa".repeat(32)],
        };
        let membership = log.membership();
        let m2_seat = membership
            .members()
            .values()
            .find(|m| m.binding.recipient() == id2.recipient())
            .expect("m2 seat");
        assert!(!member_denied_by_roster(&both, m2_seat));
        assert!(!recipient_denied_by_roster_or_log(
            Some(&both),
            &log,
            &id2.recipient()
        ));

        // Negative control: denying `m8` as well makes both functions return true.
        both.deny_labels.push("m8".to_owned());
        assert!(member_denied_by_roster(&both, m2_seat));
        assert!(recipient_denied_by_roster_or_log(
            Some(&both),
            &log,
            &id2.recipient()
        ));

        // 2. `merge_swarm_roster` with stale denied in `acc` and active in `other` replaces the
        //    stale denied row without polluting `m8`'s aliases with `"m5"`.
        let mut acc1 = SwarmRoster {
            members: vec![stale_denied.clone()],
            deny_labels: vec!["m5".to_owned()],
            deny_nodes: vec!["aa".repeat(32)],
        };
        let other1 = SwarmRoster {
            members: vec![active_row.clone()],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };
        merge_swarm_roster(&mut acc1, other1);
        let merged1 = acc1.member_by_recipient(&rec2_str).expect("merged1");
        assert_eq!(merged1.label, "m8");
        assert!(!acc1.is_member_denied(merged1));

        // 3. `merge_swarm_roster` in the reverse direction (`active` in `acc`, `stale_denied` in
        //    `other`) also keeps `m8` un-denied.
        let mut acc2 = SwarmRoster {
            members: vec![active_row],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };
        let other2 = SwarmRoster {
            members: vec![stale_denied],
            deny_labels: vec!["m5".to_owned()],
            deny_nodes: vec!["aa".repeat(32)],
        };
        merge_swarm_roster(&mut acc2, other2);
        let merged2 = acc2.member_by_recipient(&rec2_str).expect("merged2");
        assert_eq!(merged2.label, "m8");
        assert!(!acc2.is_member_denied(merged2));
    }

    #[test]
    fn member_node_removed_and_denied_checks_cover_binding_writer_and_incarnation() {
        let id1 = Identity::from_secret_bytes([0x11; 32]);
        let id2 = Identity::from_secret_bytes([0x22; 32]);
        let id3 = Identity::from_secret_bytes([0x33; 32]);
        let req1 =
            Request::create([2; 16], [3; 32], [1; 16], [1; 16], &id1, &[1; 32]).expect("req1");
        let req2 =
            Request::create([2; 16], [3; 32], [2; 16], [0x2a; 16], &id2, &[2; 32]).expect("req2");
        let req3 =
            Request::create([2; 16], [3; 32], [3; 16], [0x3a; 16], &id3, &[3; 32]).expect("req3");
        let m1_actor = req1.binding().actor();
        let m2_actor = req2.binding().actor();
        let m2_writer = *req2.binding().writer();
        let m2_incarnation = *req2.binding().incarnation();
        let m3_writer = *req3.binding().writer();
        let ent1 = Entrant {
            request: req1,
            node_key: [1; 32],
            name: "m1.local".to_owned(),
            label: "m1".to_owned(),
        };
        // `ent2` has `node_key = m3_writer`, while `ent3` is admitted with `node_key = [0; 32]`.
        // When `m2` is later removed, `membership.node(&m3_writer)` becomes `NodeStanding::Removed(m2_actor)`
        // even though `m3.node_key` is `None`.
        let ent2 = Entrant {
            request: req2,
            node_key: m3_writer,
            name: "m2.local".to_owned(),
            label: "m2".to_owned(),
        };
        let genesis = Record::found(&[99; 32], vec![ent1, ent2], 1_000).expect("genesis");
        let mut log = Log::new(genesis).expect("log");
        let ent3 = Entrant {
            request: req3,
            node_key: [0; 32],
            name: "m3.local".to_owned(),
            label: "m3".to_owned(),
        };
        let admit_m3 = log
            .draft(
                &m1_actor,
                1_001,
                crate::membership_log::Body::Admit {
                    entrant: ent3,
                    evidence: crate::membership_log::Evidence::Phrase,
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign admit m3");
        log.insert(admit_m3).expect("insert admit m3");

        // 1. Before `m2` is removed, `m2`'s `binding.writer()` or `binding.incarnation()` in
        //    `SwarmRoster::deny_nodes` denies `m2` even though `m2.node_key` (`m3_writer`) is not
        //    in `deny_nodes`.
        let m2_seat = log.membership().get(&m2_actor).expect("m2 seat").clone();
        let roster_writer_denied = SwarmRoster {
            members: Vec::new(),
            deny_labels: Vec::new(),
            deny_nodes: vec![aterm_codec::hex::encode_lower(&m2_writer)],
        };
        assert!(member_denied_by_roster(&roster_writer_denied, &m2_seat));
        assert!(recipient_denied_by_roster_or_log(
            Some(&roster_writer_denied),
            &log,
            &id2.recipient()
        ));
        let roster_inc_denied = SwarmRoster {
            members: Vec::new(),
            deny_labels: Vec::new(),
            deny_nodes: vec![aterm_codec::hex::encode_lower(&m2_incarnation)],
        };
        assert!(member_denied_by_roster(&roster_inc_denied, &m2_seat));
        assert!(recipient_denied_by_roster_or_log(
            Some(&roster_inc_denied),
            &log,
            &id2.recipient()
        ));

        // 2. Remove `m2`: `membership.node(&m3_writer)` becomes `NodeStanding::Removed(m2_actor)`,
        //    which marks `m3` (`binding.writer() == m3_writer`, `node_key == None`) as removed in
        //    `member_node_removed`, `recipient_is_removed`, and `recipient_denied_by_roster_or_log`.
        let rem_m2 = log
            .draft(
                &m1_actor,
                1_002,
                crate::membership_log::Body::Remove {
                    member: m2_actor,
                    cutoff: *log.network(),
                },
            )
            .sign(&[1; 32], [1; 16])
            .expect("sign remove m2");
        log.insert(rem_m2).expect("insert remove m2");

        let membership = log.membership();
        let m3_seat = membership
            .members()
            .values()
            .find(|m| m.binding.recipient() == id3.recipient())
            .expect("m3 seat");
        assert_eq!(m3_seat.node_key, None);
        assert!(membership.member_node_removed(m3_seat));
        assert!(log.recipient_is_removed(&id3.recipient()));
        assert!(recipient_denied_by_roster_or_log(
            None,
            &log,
            &id3.recipient()
        ));
    }
}
