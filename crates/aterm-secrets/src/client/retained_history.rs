// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent local acknowledgements of authenticated membership history.
//! Raw records remain repairable; authority reads use the acknowledged causal prefix.

use super::{
    Config, Paths, fsx, log_entries, retained_frontier::Delta, retained_frontier::Frontier,
};
use crate::membership_log::{Body, Log, RecordId, Store};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(any(test, debug_assertions))]
#[path = "retained_history_read_counts.rs"]
mod read_counts;
#[cfg(any(test, debug_assertions))]
pub use read_counts::{RetainedHistoryReadCounts, RetainedHistoryReadOutcomes};

const NAME: &str = "frontiers";
const ANCHOR: &[u8] = b"aterm-vault-retained-history/1";
const PENDING: &[u8] = b"aterm-vault-retained-history/1\0pending";
const READY: &[u8] = b"aterm-vault-retained-history/1\0ready";
const LEGACY_SUBJECT: &str = "vault.frontier";
const SUBJECT: &str = "vault.frontier/2";
const MAX_CONTEXTS: usize = 64;
const MAX_NAME: usize = 256;
const MAX_KEYS: usize = 256;
const HEADER: usize = 68;

fn limits() -> aterm_journal::Limits {
    aterm_journal::Limits {
        records: 2 * super::retained_frontier::MAX_IDS,
        bytes: crate::membership_log::MAX_RECORDS_STREAM,
        record_bytes: HEADER + MAX_NAME + 32 * MAX_KEYS + super::retained_frontier::MAX_DELTA,
    }
}

#[derive(Clone)]
struct Context {
    name: String,
    signer: [u8; 32],
    keys: BTreeSet<[u8; 32]>,
    legacy: bool,
    frontier: Frontier,
}

impl Context {
    fn matches(&self, keys: &[[u8; 32]]) -> bool {
        keys.iter().any(|key| self.keys.contains(key))
    }
}

#[derive(Clone, Default)]
struct History(BTreeMap<RecordId, Context>, usize, usize);

/// One decoded journal, bounded by the journal limits. A cache hit still asks the journal
/// to validate the current private anchor, checkpoint and every acknowledged physical byte.
struct ReadCache(
    Mutex<Option<CachedHistory>>,
    #[cfg(any(test, debug_assertions))] read_counts::Counters,
);

static CACHE: ReadCache = ReadCache::new();

/// Numeric snapshots of the process-wide cache's `ReadCache::read` calls.
/// This excludes enrollment checks and writer replay. Every journal read
/// attempts fresh physical validation; an unchanged outcome is decoded reuse,
/// not a skipped read. Join measured workers before comparing snapshots.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_retained_history_read_counts() -> RetainedHistoryReadCounts {
    CACHE.1.snapshot()
}

#[derive(Clone)]
struct CachedHistory {
    dir: PathBuf,
    prefix: aterm_journal::Prefix,
    history: Arc<History>,
}

impl ReadCache {
    const fn new() -> Self {
        Self(
            Mutex::new(None),
            #[cfg(any(test, debug_assertions))]
            read_counts::Counters::new(),
        )
    }

    fn read(&self, paths: &Paths) -> Result<Arc<History>> {
        // Do not hold a process-wide mutex across filesystem reads or replay. Concurrent
        // readers can retain their own coherent checkpoint snapshots.
        #[cfg(any(test, debug_assertions))]
        let mut lookup = read_counts::Lookup::Unavailable;
        let known = self.0.lock().ok().and_then(|entry| {
            #[cfg(any(test, debug_assertions))]
            {
                lookup = match entry.as_ref() {
                    None => read_counts::Lookup::Empty,
                    Some(held) if held.dir == paths.dir => read_counts::Lookup::SameDirectory,
                    Some(_) => read_counts::Lookup::OtherDirectory,
                };
            }
            entry
                .as_ref()
                .filter(|entry| entry.dir == paths.dir)
                .cloned()
        });
        #[cfg(any(test, debug_assertions))]
        self.1.lookup(lookup);
        let committed = aterm_journal::read_committed(
            &paths.dir,
            NAME,
            ANCHOR,
            limits(),
            known.as_ref().map(|entry| &entry.prefix),
        );
        #[cfg(any(test, debug_assertions))]
        self.1.journal_result(lookup, &committed);
        let committed = committed?;
        let (records, prefix, mut history) = match committed {
            aterm_journal::Committed::Unchanged => {
                return known
                    .map(|entry| entry.history)
                    .ok_or_else(|| Error::new("missing retained receipt cache"));
            }
            aterm_journal::Committed::Extended { records, prefix } => (
                records,
                prefix,
                known
                    .ok_or_else(|| Error::new("missing retained receipt prefix"))?
                    .history,
            ),
            aterm_journal::Committed::Rebuilt { records, prefix } => {
                (records, prefix, Arc::new(History::default()))
            }
        };
        let replayed = Arc::make_mut(&mut history);
        for record in records {
            #[cfg(any(test, debug_assertions))]
            self.1.replay_attempt();
            let accepted = replayed.ingest_record(&record.subject, &record.body);
            #[cfg(any(test, debug_assertions))]
            self.1.replay_result(accepted.is_ok());
            accepted?;
        }
        replayed.2 = usize::try_from(prefix.acknowledged().length)
            .map_err(|_| Error::new("retained journal length exceeds address space"))?;
        // Publish only after every receipt passed. A malformed suffix cannot alter any
        // earlier reader's snapshot or become the prefix used on the next attempt.
        if let Ok(mut entry) = self.0.lock() {
            #[cfg(any(test, debug_assertions))]
            let replaces_other_directory = entry.as_ref().is_some_and(|held| held.dir != paths.dir);
            *entry = Some(CachedHistory {
                dir: paths.dir.clone(),
                prefix,
                history: history.clone(),
            });
            #[cfg(any(test, debug_assertions))]
            self.1.publish(replaces_other_directory);
        }
        Ok(history)
    }

    /// Opening the exclusive journal already checked its physical checkpoint. Reuse only
    /// the same decoded prefix, with the writer's current custody still intact.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRetainedReplay",
            action = "OtherDirectory",
            reason = "The caller opens a journal under a different directory key"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRetainedReplay",
            action = "Advance",
            reason = "An earlier committed append changes the checkpoint before this writer opens"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRetainedReplay",
            action = "LoseCustody",
            reason = "The environment invalidates the opened writer's files or lease"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "VaultRetainedReplay",
            action = "Evict",
            reason = "Another reader replaces the bounded cache entry before writer replay"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "VaultRetainedReplay",
            action = "Replay",
            project = "aterm_secrets::client::retained_history::read_tests::writer_replay_transition"
        )
    )]
    fn replay(&self, journal: &aterm_journal::Log) -> Result<Arc<History>> {
        if journal.is_poisoned() {
            return Err(Error::new("uncertain retained journal: reopen"));
        }
        let acknowledged = journal.acknowledged();
        if let Ok(entry) = self.0.lock()
            && let Some(entry) = entry.as_ref()
            && entry.dir.as_path() == journal.directory()
            && entry.prefix.acknowledged() == acknowledged
        {
            return Ok(entry.history.clone());
        }
        replay_uncached(journal).map(Arc::new)
    }
}

impl History {
    fn ingest_record(&mut self, subject: &str, bytes: &[u8]) -> Result<()> {
        if subject == SUBJECT {
            return self.ingest(bytes);
        }
        if subject != LEGACY_SUBJECT || bytes.len() < 66 {
            return Err(Error::new("unknown retained membership receipt"));
        }
        let network: RecordId = bytes[..32]
            .try_into()
            .map_err(|_| Error::new("invalid legacy receipt"))?;
        let name_len = u16::from_le_bytes([bytes[64], bytes[65]]) as usize;
        if name_len == 0 || name_len > MAX_NAME || bytes.len() < 66 + name_len {
            return Err(Error::new("invalid legacy receipt"));
        }
        // V1 attested the genesis signer, but no further key associations. Preserve exactly
        // that evidence; a later verified configuration can add a V2 binding receipt.
        let first = !self.0.contains_key(&network);
        let legacy = self.0.get(&network).is_none_or(|context| context.legacy);
        let mut upgraded = Vec::with_capacity(bytes.len() + 2 + if first { 32 } else { 0 });
        upgraded.extend_from_slice(&bytes[..66]);
        upgraded.extend_from_slice(&u16::from(first).to_le_bytes());
        upgraded.extend_from_slice(&bytes[66..66 + name_len]);
        if first {
            upgraded.extend_from_slice(&bytes[32..64]);
        }
        upgraded.extend_from_slice(&bytes[66 + name_len..]);
        self.ingest(&upgraded)?;
        self.0
            .get_mut(&network)
            .ok_or_else(|| Error::new("missing legacy receipt context"))?
            .legacy = legacy;
        Ok(())
    }

    fn ingest(&mut self, bytes: &[u8]) -> Result<()> {
        let invalid = || Error::new("invalid retained membership receipt");
        if bytes.len() < HEADER {
            return Err(invalid());
        }
        let network: RecordId = bytes[..32].try_into().map_err(|_| invalid())?;
        let signer: [u8; 32] = bytes[32..64].try_into().map_err(|_| invalid())?;
        let name_len =
            u16::from_le_bytes(bytes[64..66].try_into().map_err(|_| invalid())?) as usize;
        let key_count =
            u16::from_le_bytes(bytes[66..68].try_into().map_err(|_| invalid())?) as usize;
        let delta_at = HEADER + name_len + 32 * key_count;
        if name_len == 0 || name_len > MAX_NAME || key_count > MAX_KEYS || bytes.len() < delta_at {
            return Err(invalid());
        }
        let name = std::str::from_utf8(&bytes[HEADER..HEADER + name_len]).map_err(|_| invalid())?;
        let keys = bytes[HEADER + name_len..delta_at].as_chunks::<32>().0;
        if keys.windows(2).any(|pair| pair[0] >= pair[1]) || keys.contains(&[0; 32]) {
            return Err(invalid());
        }
        let delta = (bytes.len() > delta_at)
            .then(|| Delta::decode(&bytes[delta_at..]))
            .transpose()?;
        if !self.0.contains_key(&network) {
            if delta.is_none() || !keys.contains(&signer) {
                return Err(invalid());
            }
            if self.0.len() >= MAX_CONTEXTS {
                return Err(Error::new("retained membership context capacity exhausted"));
            }
            self.0.insert(
                network,
                Context {
                    name: name.to_owned(),
                    signer,
                    keys: BTreeSet::new(),
                    legacy: false,
                    frontier: Frontier::empty(network),
                },
            );
        }
        let context = self.0.get_mut(&network).ok_or_else(invalid)?;
        if context.name != name
            || context.signer != signer
            || context.keys.len() + keys.len() > MAX_KEYS
            || keys.iter().any(|key| context.keys.contains(key))
            || (delta.is_none() && keys.is_empty())
        {
            return Err(invalid());
        }
        if let Some(delta) = delta {
            context.frontier.apply(&delta)?;
        }
        context.keys.extend(keys.iter().copied());
        context.legacy = false;
        self.1 += 1;
        Ok(())
    }

    fn context(&self, paths: &Paths, config: &Config, log: &Log) -> Result<Option<&Context>> {
        if let Some(held) = self.0.get(log.network()) {
            let direct = held.matches(&configured_keys(config));
            if held.name != config.name
                || replaced(paths, config, held)
                || (!direct
                    && compiled_network(config) != Some(*log.network())
                    && !held.matches(&lineage(paths, config)))
            {
                return Err(Error::new(
                    "retained membership context is foreign or retired",
                ));
            }
            return Ok(Some(held));
        }
        let keys = lineage(paths, config);
        if self
            .related(config, &keys)
            .any(|old| !replaced(paths, config, old))
        {
            return Err(Error::new(
                "another genesis cannot reset retained membership history",
            ));
        }
        Ok(None)
    }

    fn related<'a>(
        &'a self,
        config: &'a Config,
        keys: &'a [[u8; 32]],
    ) -> impl Iterator<Item = &'a Context> {
        self.0.values().filter(move |old| {
            old.name == config.name
                && (old.legacy
                    || old.matches(keys)
                    || compiled_network(config) == Some(*old.frontier.network()))
        })
    }

    fn plan(
        &self,
        paths: &Paths,
        config: &Config,
        log: &Log,
        replacement: bool,
    ) -> Result<Option<Vec<u8>>> {
        validate_name(&config.name)?;
        let context = self.context(paths, config, log)?;
        let Body::Genesis { signer, .. } = log.genesis().body() else {
            return Ok(None);
        };
        let keys = if context.is_some() {
            configured_keys(config)
        } else {
            lineage(paths, config)
        };
        if context.is_none()
            && (!keys.contains(signer)
                || (!replacement && !log_entries::check_legacy_frontier(paths, config, log)?))
        {
            return Ok(None);
        }
        let fresh = Frontier::empty(*log.network());
        let frontier = context.map_or(&fresh, |held| &held.frontier);
        if !super::retained_frontier::covers(log, log.network(), &frontier.heads()) {
            return Ok(None);
        }
        let keys: Vec<_> = keys
            .into_iter()
            .filter(|key| context.is_none_or(|held| !held.keys.contains(key)))
            .collect();
        if context.map_or(0, |held| held.keys.len()) + keys.len() > MAX_KEYS {
            return Err(Error::new("retained membership key capacity exhausted"));
        }
        let delta = frontier.plan(log)?;
        if delta.is_none() && keys.is_empty() {
            return Ok(None);
        }
        if context.is_none() && self.0.len() >= MAX_CONTEXTS {
            return Err(Error::new("retained membership context capacity exhausted"));
        }
        let delta = delta.map_or_else(Vec::new, |delta| delta.encode());
        let name = context.map_or(config.name.as_str(), |held| held.name.as_str());
        let mut bytes = Vec::with_capacity(HEADER + name.len() + 32 * keys.len() + delta.len());
        bytes.extend_from_slice(log.network());
        bytes.extend_from_slice(signer);
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&(keys.len() as u16).to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        for key in keys {
            bytes.extend_from_slice(&key);
        }
        bytes.extend_from_slice(&delta);
        let framed = bytes
            .len()
            .saturating_add(SUBJECT.len())
            .saturating_add(aterm_journal::APPEND_OVERHEAD);
        if self.1 >= limits().records || self.2.saturating_add(framed) > limits().bytes {
            return Err(Error::new("retained membership journal capacity exhausted"));
        }
        Ok(Some(bytes))
    }
}

fn configured_keys(config: &Config) -> Vec<[u8; 32]> {
    let mut keys: Vec<_> = config
        .signer
        .into_iter()
        .chain(config.followed.iter().copied())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// The configured key line plus a cryptographically verified cached handoff. A disconnected
/// compiled replacement is a different context, not an endorsement of the retired line.
pub(super) fn lineage(paths: &Paths, config: &Config) -> Vec<[u8; 32]> {
    let mut keys = configured_keys(config);
    if let Some(pin) = config.signer
        && let Ok(Some(bytes)) =
            fsx::read_private(&paths.cache_file(), crate::envelope::MAX_ENVELOPE)
        && let Ok((envelope, pin, _)) = crate::envelope::verify_for_reader(
            bytes.as_slice(),
            &config.name,
            &pin,
            &config.followed,
            None,
        )
        && !matches!(pin, crate::envelope::Pin::Replaced(_))
    {
        keys.push(envelope.signer);
        keys.extend(envelope.reached_from());
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn compiled_network(config: &Config) -> Option<RecordId> {
    (config.name == crate::ALAB_VAULT && super::alab_vault(config))
        .then(|| crate::membership_log::genesis::compiled().ok().flatten())
        .flatten()
}

fn replaced(paths: &Paths, config: &Config, old: &Context) -> bool {
    if let Some(network) = compiled_network(config) {
        return &network != old.frontier.network();
    }
    let retired = crate::envelope::Retired::compiled();
    retired.gives_way(&old.signer, &[])
        && config.signer == retired.current
        && !config.followed.contains(&old.signer)
        // An authenticated cached handoff retains custody of the old genesis even when
        // config has already moved to the new signer without its followed list.
        && !lineage(paths, config).contains(&old.signer)
        && !old.matches(&configured_keys(config))
}

fn claim(paths: &Paths) -> std::path::PathBuf {
    paths.dir.join("records.frontiers")
}

/// The journal's file of `suffix`, as `aterm_journal` lays out the journal [`NAME`].
fn journal_file(paths: &Paths, suffix: &str) -> PathBuf {
    paths.dir.join(format!("{NAME}.{suffix}"))
}

/// Every file a read of the acknowledged history reads ([`Paths::acknowledged_history_files`]):
/// the enrollment claim, then the journal's anchor, its receipts and its checkpoint, which each
/// acknowledgement replaces.
pub(super) fn files(paths: &Paths) -> [PathBuf; 4] {
    [
        claim(paths),
        journal_file(paths, "anchor"),
        journal_file(paths, "astream"),
        journal_file(paths, "checkpoint"),
    ]
}

fn established(paths: &Paths) -> Result<bool> {
    match fsx::read_private(&claim(paths), PENDING.len())? {
        Some(bytes)
            if bytes.as_slice() == PENDING
                || bytes.as_slice() == READY
                || bytes.as_slice() == ANCHOR =>
        {
            Ok(true)
        }
        Some(_) => Err(Error::new("invalid retained membership enrollment")),
        None => Ok(false),
    }
}

fn read(paths: &Paths) -> Result<Arc<History>> {
    read_history(paths, false)
}

fn ready(paths: &Paths) -> Result<bool> {
    established(paths)?;
    Ok(fsx::read_private(&claim(paths), PENDING.len())?
        .is_some_and(|bytes| bytes.as_slice() == READY || bytes.as_slice() == ANCHOR))
}

fn read_history(paths: &Paths, allow_pending: bool) -> Result<Arc<History>> {
    if !established(paths)? {
        if fsx::kind(&journal_file(paths, "anchor"))? != fsx::Kind::Missing {
            return Err(Error::new(
                "retained membership enrollment needs exclusive recovery",
            ));
        }
        return Ok(Arc::new(History::default()));
    }
    let ready = ready(paths)?;
    let history = CACHE.read(paths)?;
    if ready && history.0.is_empty() {
        return Err(Error::new(
            "established membership journal lost its initial receipt",
        ));
    }
    if !ready && !allow_pending && !history.0.is_empty() {
        return Err(Error::of_kind(
            crate::ErrorKind::MembershipHistoryUnavailable,
            "initial membership enrollment needs exclusive recovery",
        ));
    }
    // An empty pending attempt has acknowledged no log authority. It does not prevent an
    // independent envelope-only context from reading its existing envelope; a candidate log
    // still fails the missing-context check until its first receipt becomes ready.
    Ok(history)
}

/// Exact-root custody also constrains raw recovery, so a competing genesis cannot displace
/// the records needed for repair. An explicit compiled replacement selects a new context.
pub(super) fn pinned_network(paths: &Paths, config: &Config) -> Result<Option<RecordId>> {
    let history = read_history(paths, true)?;
    let mut named = history
        .0
        .iter()
        .filter(|(_, context)| context.name == config.name);
    let Some((network, context)) = named.next() else {
        return Ok(None);
    };
    if named.next().is_none()
        && (context.matches(&configured_keys(config)) || compiled_network(config) == Some(*network))
        && !replaced(paths, config, context)
    {
        // The sole context is already pinned directly. A new competing genesis still
        // takes the full lineage check in History::context before it can be retained.
        return Ok(Some(*network));
    }
    let keys = lineage(paths, config);
    let mut matching = history.0.iter().filter(|(_, old)| {
        old.name == config.name
            && (old.matches(&keys) || compiled_network(config) == Some(*old.frontier.network()))
            && !replaced(paths, config, old)
    });
    let network = matching.next().map(|(network, _)| *network);
    if matching.next().is_some() {
        return Err(Error::new(
            "multiple retained membership roots match the configured lineage",
        ));
    }
    if network.is_none()
        && history.0.values().any(|context| {
            context.name == config.name && context.legacy && !replaced(paths, config, context)
        })
    {
        return Err(Error::of_kind(
            crate::ErrorKind::MembershipHistoryUnavailable,
            "legacy retained history needs authenticated key-lineage evidence",
        ));
    }
    Ok(network)
}

/// A completed receipt independently preserves the configured key's association with this
/// genesis. A transient cached handoff alone is not this durable evidence.
pub(super) fn durably_bound(paths: &Paths, config: &Config, network: &RecordId) -> Result<bool> {
    Ok(read(paths)?.0.get(network).is_some_and(|context| {
        context.name == config.name
            && context.matches(&configured_keys(config))
            && !replaced(paths, config, context)
    }))
}

/// Receipt changes invalidate health and setup observations even when raw records did not move.
pub(super) fn version(paths: &Paths) -> Result<[u8; 32]> {
    let history = read(paths)?;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(ANCHOR);
    for (network, context) in &history.0 {
        hash.update(network);
        hash.update(context.frontier.digest());
        hash.update([u8::from(context.legacy)]);
        hash.update((context.keys.len() as u16).to_le_bytes());
        for key in &context.keys {
            hash.update(key);
        }
    }
    Ok(hash.finalize())
}

/// Complete a first enrollment interrupted after its journal anchor was made durable but
/// before the independent claim. No receipt can precede the claim in the writer protocol.
pub(super) fn recover_enrollment(paths: &Paths) -> Result<()> {
    let enrolled = established(paths)?;
    if ready(paths)? {
        let _ = read_history(paths, true)?;
        return Ok(());
    }
    if !enrolled && fsx::kind(&journal_file(paths, "anchor"))? == fsx::Kind::Missing {
        return Ok(());
    }
    let mut journal = aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, true, limits())?;
    let history = replay(&journal)?;
    if !enrolled && !history.0.is_empty() {
        return Err(Error::new(
            "acknowledged membership history lost its enrollment claim",
        ));
    }
    journal.finish_open()?;
    let marker = if history.0.is_empty() { PENDING } else { READY };
    aterm_journal::write_private_durable(&paths.dir, "records.frontiers", marker)?;
    Ok(())
}

fn replay(journal: &aterm_journal::Log) -> Result<Arc<History>> {
    CACHE.replay(journal)
}

fn replay_uncached(journal: &aterm_journal::Log) -> Result<History> {
    let mut history = History(BTreeMap::new(), 0, journal.bytes());
    let mut offset = astream_wire::Offset::ZERO;
    loop {
        let records = journal.read_range(offset, 256)?;
        if records.is_empty() {
            break;
        }
        for record in &records {
            history.ingest_record(&record.subject, &record.body)?;
        }
        offset = astream_wire::Offset(offset.0 + records.len() as u64);
    }
    Ok(history)
}

/// Called only while holding the exclusive state lock, after raw records are durable.
/// Opening completes a previous uncertain journal append before planning its next delta.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultRetainedCommit",
        action = "Acknowledge",
        project = "aterm_secrets::client::retained_history_tests::retained_commit_transition"
    )
)]
pub(super) fn acknowledge(paths: &Paths, store: &Store) -> Result<()> {
    let Some(config) = super::config(paths)? else {
        return Ok(());
    };
    acknowledge_selected(paths, store, &config, false)
}

/// An exclusive caller may supply a configuration authenticated by an explicit replacement
/// envelope before publishing that configuration or materializing its values.
pub(super) fn acknowledge_replacement(paths: &Paths, store: &Store, config: &Config) -> Result<()> {
    acknowledge_selected(paths, store, config, true)
}

pub(super) fn acknowledge_for(paths: &Paths, store: &Store, config: &Config) -> Result<()> {
    acknowledge_selected(paths, store, config, false)
}

fn acknowledge_selected(
    paths: &Paths,
    store: &Store,
    config: &Config,
    replacement: bool,
) -> Result<()> {
    let Some(log) = store.log() else {
        return Ok(());
    };
    if !eligible(config) {
        return Ok(());
    }
    let enrolled = established(paths)?;
    let before = if enrolled {
        read_history(paths, true)?
    } else {
        Arc::new(History::default())
    };
    if before.plan(paths, config, log, replacement)?.is_none() {
        return Ok(());
    }
    let mut journal = aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, enrolled, limits())?;
    let mut history = replay(&journal)?;
    let Some(bytes) = history.plan(paths, config, log, replacement)? else {
        return Ok(());
    };
    // Key associations and the frontier are acknowledged in the same journal frame.
    Arc::make_mut(&mut history).ingest(&bytes)?;
    journal.finish_open()?;
    // All deterministic validation precedes either enrollment marker.
    if !enrolled {
        aterm_journal::write_private_durable(&paths.dir, "records.frontiers", PENDING)?;
    }
    journal.append(SUBJECT, bytes)?;
    if !history.0.is_empty() && !ready(paths)? {
        aterm_journal::write_private_durable(&paths.dir, "records.frontiers", READY)?;
    }
    Ok(())
}

/// Detect deterministic context and delta refusals before replacing raw records. A partial
/// candidate is still accepted for repair, but cannot lower or advance its retained floor.
pub(super) fn validate_write(paths: &Paths, store: &Store) -> Result<()> {
    let Some(config) = super::config(paths)? else {
        return Ok(());
    };
    validate_configured(paths, &config, store, false)
}

/// [`validate_write`] of `store` under `config`, a configuration a member's bundle is about to
/// save, so a bundle whose records this history refuses under it is refused before anything is
/// written. `afresh` where that write first forgets the version and the digests the state holds
/// of the vault before (`client::forget_vault_left`): the acknowledgement that follows then needs
/// no applied frontier, as an explicit replacement's does not, so the check is that one's.
pub(super) fn validate_configured(
    paths: &Paths,
    config: &Config,
    store: &Store,
    afresh: bool,
) -> Result<()> {
    validate_name(&config.name)?;
    let Some(log) = store.log() else {
        return Ok(());
    };
    let history = read_history(paths, true)?;
    if eligible(config) {
        let _ = history.plan(paths, config, log, afresh)?;
    } else {
        let _ = history.context(paths, config, log)?;
    }
    Ok(())
}

fn eligible(config: &Config) -> bool {
    config.signer.is_some() && super::pin_admitted(config, None)
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(Error::new(
            "retained membership vault name exceeds its bound",
        ));
    }
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultRetainedCommit",
        action = "Publish",
        reason = "Environment finishes a raw records publication before committing its receipt"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultRetainedCommit",
        action = "Rollback",
        reason = "Environment replaces raw records with a previously valid signed snapshot"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultRetainedCommit",
        action = "Damage",
        reason = "Environment damages an independently retained checkpoint"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultRetainedCommit",
        action = "Read",
        project = "aterm_secrets::client::retained_history_tests::retained_commit_transition"
    )
)]
pub(super) fn project_store(paths: &Paths, config: &Config, store: Store) -> Result<Store> {
    project_selected(paths, config, store, false)
}

/// Inspect a verified replacement's candidate for causal completeness, without granting
/// current authority to an unacknowledged new context. Existing contexts still enforce F.
pub(super) fn replacement_candidate(paths: &Paths, config: &Config, store: Store) -> Result<Store> {
    project_selected(paths, config, store, true)
}

fn project_selected(
    paths: &Paths,
    config: &Config,
    store: Store,
    replacement: bool,
) -> Result<Store> {
    let history = read(paths)?;
    let Some(log) = store.log() else {
        let keys = lineage(paths, config);
        if history
            .related(config, &keys)
            .any(|old| !replaced(paths, config, old))
        {
            return Err(Error::of_kind(
                crate::ErrorKind::MembershipHistoryUnavailable,
                "retained membership records are missing",
            ));
        }
        return Ok(store);
    };
    let Some(context) = history.context(paths, config, log)? else {
        if !replacement && established(paths)? {
            return Err(Error::of_kind(
                crate::ErrorKind::MembershipHistoryUnavailable,
                "membership history awaits its first durable acknowledgement",
            ));
        }
        if !replacement
            && !log_entries::legacy_applied_log_available(&super::load_state(paths)?, log)
        {
            return Err(Error::of_kind(
                crate::ErrorKind::MembershipHistoryUnavailable,
                "the legacy applied membership frontier is unavailable",
            ));
        }
        return Ok(store);
    };
    let projected = context.frontier.project(log)?;
    if projected.heads() == log.heads() {
        return Ok(store);
    }
    store.projected(projected)
}

/// Uncommitted drafts may extend retained history but may never omit it.
pub(super) fn require_covers(paths: &Paths, config: &Config, log: &Log) -> Result<()> {
    if let Some(context) = read(paths)?.context(paths, config, log)? {
        context.frontier.project(log)?;
    } else if established(paths)? {
        return Err(Error::of_kind(
            crate::ErrorKind::MembershipHistoryUnavailable,
            "membership history awaits its first durable acknowledgement",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "retained_history_read_tests.rs"]
mod read_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_receipts_keep_their_prefix_when_new_receipts_are_appended() {
        let _answering = crate::client::ServersAnswering::set();
        let lab = crate::client::log_entries_tests::two_macs(&[(
            crate::text::Audience::Everyone,
            "shared",
            b"old",
        )]);
        let old = crate::client::load_records(&lab.m7).unwrap();
        let config = crate::client::configured(&lab.m7).unwrap();
        let scratch = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(scratch.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        crate::client::save_config(&paths, &config).unwrap();
        fsx::write_atomic(&paths.records_file(), &old.encode(), 0o600).unwrap();
        let log = old.log().unwrap();
        let Body::Genesis { signer, .. } = log.genesis().body() else {
            unreachable!()
        };
        let mut legacy = Vec::new();
        legacy.extend_from_slice(log.network());
        legacy.extend_from_slice(signer);
        legacy.extend_from_slice(&(config.name.len() as u16).to_le_bytes());
        legacy.extend_from_slice(config.name.as_bytes());
        legacy.extend_from_slice(
            &Frontier::empty(*log.network())
                .plan(log)
                .unwrap()
                .unwrap()
                .encode(),
        );
        let mut journal =
            aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, false, limits()).unwrap();
        journal.finish_open().unwrap();
        journal.append(LEGACY_SUBJECT, legacy.clone()).unwrap();
        drop(journal);
        fsx::write_atomic(&claim(&paths), ANCHOR, 0o600).unwrap();
        let mut unproven = config.clone();
        unproven.signer = Some([55; 32]);
        unproven.followed.clear();
        assert!(
            pinned_network(&paths, &unproven).is_err(),
            "an old receipt with no adopting-key evidence cannot be silently abandoned"
        );
        assert!(project_store(&paths, &unproven, Store::with_compiled(None)).is_err());
        assert!(project_store(&paths, &config, old).is_ok());

        let admin = crate::client::admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&crate::text::Audience::Everyone, "shared", b"new")
            .unwrap();
        crate::client::admin_publish(&lab.m22, &admin, &doc).unwrap();
        let new = crate::client::load_records(&lab.m22).unwrap();
        {
            let _state = crate::client::StateLock::take(&paths).unwrap();
            crate::client::write_records(&paths, &new).unwrap();
        }
        let journal = aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, true, limits()).unwrap();
        let records = journal.read_range(astream_wire::Offset::ZERO, 2).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].subject, LEGACY_SUBJECT);
        assert_eq!(records[0].body, legacy);
        assert_eq!(records[1].subject, SUBJECT);
        assert!(project_store(&paths, &config, new).is_ok());
        lab.server.stop();
    }

    #[test]
    fn pending_enrollment_recovers_but_ready_empty_history_refuses() {
        let _answering = crate::client::ServersAnswering::set();
        let fixture = crate::client::log_entries_tests::two_macs(&[]);
        let source = crate::client::load_records(&fixture.m7).unwrap();
        let config = crate::client::configured(&fixture.m7).unwrap();
        let invalid = aterm_tempfile::tempdir().unwrap();
        let invalid_paths = Paths::for_home(invalid.path());
        fsx::ensure_private_dir(&invalid_paths.dir).unwrap();
        let mut invalid_config = config.clone();
        invalid_config.name = "x".repeat(MAX_NAME + 1);
        assert!(acknowledge_for(&invalid_paths, &source, &invalid_config).is_err());
        assert_eq!(
            fsx::kind(&invalid_paths.dir.join("frontiers.anchor")).unwrap(),
            fsx::Kind::Missing
        );
        assert_eq!(
            fsx::kind(&claim(&invalid_paths)).unwrap(),
            fsx::Kind::Missing
        );
        for committed in [false, true] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(scratch.path());
            fsx::ensure_private_dir(&paths.dir).unwrap();
            crate::client::save_config(&paths, &config).unwrap();
            fsx::write_atomic(&paths.records_file(), &source.encode(), 0o600).unwrap();
            let mut journal =
                aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, false, limits()).unwrap();
            journal.finish_open().unwrap();
            drop(journal);
            fsx::write_atomic(
                &claim(&paths),
                if committed { READY } else { PENDING },
                0o600,
            )
            .unwrap();
            assert!(project_store(&paths, &config, source.clone()).is_err());
            if !committed {
                assert!(
                    project_store(&paths, &config, Store::with_compiled(None))
                        .unwrap()
                        .log()
                        .is_none(),
                    "an empty pending attempt cannot poison an independent envelope-only context"
                );
            }
            let repaired = crate::client::StateLock::take(&paths);
            if committed {
                assert!(
                    repaired.is_err(),
                    "empty replacement cannot reset established history"
                );
                assert_eq!(
                    fsx::read_private(&claim(&paths), PENDING.len())
                        .unwrap()
                        .unwrap()
                        .as_slice(),
                    READY
                );
            } else {
                drop(repaired.unwrap());
                assert!(ready(&paths).unwrap());
                assert!(project_store(&paths, &config, source.clone()).is_ok());
                // A crash after the first delta but before its ready marker can finish locally.
                fsx::write_atomic(&claim(&paths), PENDING, 0o600).unwrap();
                assert!(project_store(&paths, &config, source.clone()).is_err());
                drop(crate::client::StateLock::take(&paths).unwrap());
                assert!(project_store(&paths, &config, source.clone()).is_ok());
            }
        }
        fixture.server.stop();
    }

    #[test]
    fn switching_vaults_retains_each_genesis_without_reusing_its_frontier() {
        let _answering = crate::client::ServersAnswering::set();
        let a = crate::client::log_entries_tests::two_macs(&[]);
        let b = crate::client::log_entries_tests::two_macs(&[]);
        let ca = crate::client::configured(&a.m7).unwrap();
        let cb = crate::client::configured(&b.m7).unwrap();
        let sa = crate::client::load_records(&a.m7).unwrap();
        let sb = crate::client::load_records(&b.m7).unwrap();
        let scratch = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(scratch.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        crate::client::save_config(&paths, &ca).unwrap();
        crate::client::write_records(&paths, &sa).unwrap();
        let mut other_name = ca.clone();
        other_name.name = "another-vault".into();
        assert!(project_store(&paths, &other_name, sa.clone()).is_err());
        other_name.signer = None;
        other_name.followed.clear();
        crate::client::save_config(&paths, &other_name).unwrap();
        let no_pin = crate::client::load_records(&paths).unwrap();
        assert!(
            no_pin.log().is_none(),
            "an envelope-only source cannot inherit another vault's raw authority"
        );
        assert!(project_store(&paths, &other_name, no_pin).is_ok());
        crate::client::save_config(&paths, &cb).unwrap();
        assert!(project_store(&paths, &cb, sa.clone()).is_err());
        assert!(
            crate::client::load_records(&paths).unwrap().log().is_none(),
            "the new key line cannot load the previous vault's raw root"
        );
        crate::client::write_records(&paths, &sb).unwrap();
        assert_eq!(read(&paths).unwrap().0.len(), 2);
        assert_eq!(
            pinned_network(&paths, &ca).unwrap(),
            Some(*sa.log().unwrap().network())
        );
        assert_eq!(
            pinned_network(&paths, &cb).unwrap(),
            Some(*sb.log().unwrap().network())
        );
        crate::client::save_config(&paths, &ca).unwrap();
        assert!(project_store(&paths, &ca, crate::client::load_records(&paths).unwrap()).is_err());
        crate::client::save_records(&paths, &sa.encode()).unwrap();
        assert!(project_store(&paths, &ca, crate::client::load_records(&paths).unwrap()).is_ok());
        assert_eq!(
            read(&paths).unwrap().0.len(),
            2,
            "returning to A cannot forget B"
        );
        a.server.stop();
        b.server.stop();
    }

    #[test]
    fn cached_endorsement_pins_a_new_signers_genesis_before_config_repinning() {
        let _answering = crate::client::ServersAnswering::set();
        let lab = crate::client::log_entries_tests::two_macs(&[]);
        let other = crate::client::log_entries_tests::two_macs(&[]);
        let admin = crate::client::admin_open(&lab.m22).unwrap();
        let source = crate::client::load_records(&lab.m7).unwrap();
        let foreign = crate::client::load_records(&other.m7).unwrap();
        let old_seed = [53; 32];
        let mut config = crate::client::configured(&lab.m7).unwrap();
        config.signer = Some(crate::envelope::signer_public(&old_seed).unwrap());
        config.followed.clear();
        let scratch = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(scratch.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        crate::client::save_config(&paths, &config).unwrap();
        let envelope = crate::envelope::sign_unchecked(
            &crate::envelope::Unsigned {
                name: config.name.clone(),
                seq: 1,
                time: 1,
                handoff: crate::envelope::test_chain(&config.name, &[old_seed, admin.seed]),
                parents: Vec::new(),
                lockers: vec![crate::envelope::Locker {
                    label: "admin".into(),
                    armored: crate::envelope::test_armor(b"inert lineage witness"),
                }],
            },
            &admin.seed,
        )
        .unwrap();
        fsx::write_atomic(&paths.cache_file(), &envelope, 0o600).unwrap();
        crate::client::write_records(&paths, &source).unwrap();
        assert!(ready(&paths).unwrap());
        assert_eq!(
            pinned_network(&paths, &config).unwrap(),
            Some(*source.log().unwrap().network())
        );
        let before = std::fs::read(paths.records_data_file().unwrap()).unwrap();
        assert!(crate::client::write_records(&paths, &foreign).is_err());
        assert_eq!(
            std::fs::read(paths.records_data_file().unwrap()).unwrap(),
            before
        );
        std::fs::rename(paths.cache_file(), paths.dir.join("cache.saved")).unwrap();
        assert_eq!(
            pinned_network(&paths, &config).unwrap(),
            Some(*source.log().unwrap().network()),
            "cache loss cannot forget the key that admitted this retained genesis"
        );
        assert!(project_store(&paths, &config, Store::with_compiled(None)).is_err());
        assert!(
            crate::client::log_entries::trusted_configured_log(&paths, &config)
                .unwrap()
                .is_some()
        );
        assert!(
            crate::client::log_entries::commons_log_matches_local_lineage(
                &paths,
                source.log().unwrap()
            )
            .unwrap()
        );
        let Body::Genesis { root, founders, .. } = source.log().unwrap().genesis().body() else {
            unreachable!();
        };
        let older =
            crate::membership_log::Record::genesis(&old_seed, *root, founders.clone(), 1).unwrap();
        let mut older_store = Store::with_compiled(None);
        older_store.ingest(older.bytes()).unwrap();
        assert!(crate::client::write_records(&paths, &older_store).is_err());
        assert_eq!(
            std::fs::read(paths.records_data_file().unwrap()).unwrap(),
            before
        );
        lab.server.stop();
        other.server.stop();
    }

    #[test]
    fn a_key_only_receipt_retains_a_repin_after_cache_loss() {
        let _answering = crate::client::ServersAnswering::set();
        let lab = crate::client::log_entries_tests::two_macs(&[]);
        let admin = crate::client::admin_open(&lab.m22).unwrap();
        let source = crate::client::load_records(&lab.m7).unwrap();
        let mut config = crate::client::configured(&lab.m7).unwrap();
        let next_seed = [53; 32];
        config.signer = Some(crate::envelope::signer_public(&next_seed).unwrap());
        config.followed.clear();
        let envelope = crate::envelope::sign_unchecked(
            &crate::envelope::Unsigned {
                name: config.name.clone(),
                seq: 1,
                time: 1,
                handoff: crate::envelope::test_chain(&config.name, &[admin.seed, next_seed]),
                parents: Vec::new(),
                lockers: vec![crate::envelope::Locker {
                    label: "admin".into(),
                    armored: crate::envelope::test_armor(b"inert lineage witness"),
                }],
            },
            &next_seed,
        )
        .unwrap();
        fsx::write_atomic(&lab.m7.cache_file(), &envelope, 0o600).unwrap();
        let before = version(&lab.m7).unwrap();
        let mut full = (*read(&lab.m7).unwrap()).clone();
        let records = full.1;
        full.1 = limits().records;
        assert!(
            full.plan(&lab.m7, &config, source.log().unwrap(), false)
                .is_err()
        );
        full.1 = records;
        full.2 = limits().bytes;
        assert!(
            full.plan(&lab.m7, &config, source.log().unwrap(), false)
                .is_err()
        );
        acknowledge_for(&lab.m7, &source, &config).unwrap();
        assert_ne!(
            version(&lab.m7).unwrap(),
            before,
            "a binding-only receipt invalidates authority observations"
        );
        std::fs::rename(lab.m7.cache_file(), lab.m7.dir.join("cache.saved")).unwrap();
        assert_eq!(
            pinned_network(&lab.m7, &config).unwrap(),
            Some(*source.log().unwrap().network())
        );
        assert!(project_store(&lab.m7, &config, Store::with_compiled(None)).is_err());
        assert!(project_store(&lab.m7, &config, source).is_ok());
        lab.server.stop();
    }
}
