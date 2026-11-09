// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The selected meaning an actual local materialization applied.

use super::*;
use std::sync::Arc;

/// A private commitment to every selected entry's complete semantics, not to file ownership.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Meaning([u8; 32]);

impl std::fmt::Debug for Meaning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaterializationMeaning")
            .finish_non_exhaustive()
    }
}

impl Meaning {
    pub(super) const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub(super) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(super) fn of(entries: &[Entry]) -> Self {
        let mut entries: Vec<_> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry::fingerprint(entry)))
            .collect();
        entries.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.as_slice().cmp(y.as_slice())));
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/private-materialization-selection/1\0");
        hash.update((entries.len() as u64).to_be_bytes());
        for (_, fingerprint) in entries {
            // The existing entry encoding includes name, audience, value and both policies.
            hash.update(fingerprint.as_slice());
        }
        Self(hash.finalize())
    }
}

/// A stage-owned authenticated selection; no entry values are retained in the snapshot.
pub(super) struct Fact {
    config: Config,
    sequence: u64,
    envelope: Option<[u8; 32]>,
    records: Option<[u8; 32]>,
    source: Option<[u8; 32]>,
    membership: Stored,
    recipient: Option<Recipient>,
    inputs: Option<vault_memo::SelectionInputs>,
    pub(super) meaning: Meaning,
}

/// Current meaning and the frontier that actually selected it. An authenticated envelope
/// fallback has no selected log; an unavailable selection never constructs this reading.
pub(super) struct Current {
    pub(super) meaning: Meaning,
    pub(super) records: Option<[u8; 32]>,
}

struct Selected {
    entries: Vec<Entry>,
    recipient: Option<Recipient>,
    records: Option<[u8; 32]>,
}

impl std::fmt::Debug for Fact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaterializationSelection")
            .finish_non_exhaustive()
    }
}

/// Resolve through the same applied-source choice as the authenticated entry reader. The
/// caller holds the state lock for admission; an unlocked caller may use this only as scheduling
/// advice. This takes no lock and performs no network or materialization effects.
fn selected(paths: &Paths, config: &Config, state: &State) -> crate::Result<Option<Selected>> {
    if !pin_admitted(config, None) {
        return Ok(None);
    }
    if config.signer.is_none() {
        let Source::Path(path) = &config.source else {
            return Ok(None);
        };
        if state.membership != Stored::Plain || !source_bound(config, state) {
            return Ok(None);
        }
        return plain_entries(config, path).map(|entries| {
            Some(Selected {
                entries,
                recipient: None,
                records: None,
            })
        });
    }
    if state.membership != Stored::In {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let recipient = Some(identity.recipient());
    if state.digest.is_none() && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing {
        if !log_only_source_bound(config, state) {
            return Ok(None);
        }
        let Some(configured) = log_entries::trusted_configured_log(paths, config)? else {
            return Ok(None);
        };
        let records = Some(log_entries::records_heads_digest(&configured.log));
        return log_entries::configured_entries(&configured.log, &identity).map(|entries| {
            entries.map(|entries| Selected {
                entries,
                recipient,
                records,
            })
        });
    }
    if !source_bound(config, state) {
        return Ok(None);
    }
    let Some(bytes) = read_verified_cached_envelope(paths, config, state)? else {
        return Ok(None);
    };
    let envelope = envelope::parse(&bytes)?;
    let seeded = log_entries::trusted_seeded_log(paths, config, &envelope)?;
    if !log_entries::applied_log_available(state, seeded.as_ref().map(|(_, digest)| *digest)) {
        return Ok(None);
    }
    log_entries::select_entries_with_log(
        seeded.as_ref().map(|(log, digest)| (log, *digest)),
        &envelope,
        &identity,
        None,
    )
    .map(|selected| {
        selected.entries.map(|entries| Selected {
            entries,
            recipient,
            records: selected.log.map(|log| log.digest),
        })
    })
}

pub(super) fn current(paths: &Paths, state: &State) -> crate::Result<Option<Current>> {
    let before = vault_memo::selection_inputs(paths);
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let selected = selected(paths, &config, state)?;
    if before
        .as_ref()
        .is_some_and(|before| vault_memo::selection_inputs(paths).as_ref() != Some(before))
    {
        return Ok(None);
    }
    Ok(selected.map(|selected| Current {
        meaning: Meaning::of(&selected.entries),
        records: selected.records,
    }))
}

/// A prior fact is reused only under a current shared state observation and identical source,
/// recipient-file, history and record-budget inputs. Every caller still reads actual local files
/// and denial policy. A stamp miss reads the current selector; no plaintext is cached.
pub(super) fn observe(
    paths: &Paths,
    config: &Config,
    state: &State,
    observation: &StateLock,
    prior: Option<&Arc<Fact>>,
) -> crate::Result<Option<Arc<Fact>>> {
    if !observation.observes(paths) {
        return Ok(None);
    }
    let inputs = config
        .signer
        .is_some()
        .then(|| vault_memo::selection_inputs(paths))
        .flatten();
    if observation.observes(paths)
        && let (Some(prior), Some(inputs)) = (prior, &inputs)
        && prior.config == *config
        && prior.sequence == state.seq
        && prior.envelope == state.digest
        && prior.records == state.records_digest
        && prior.source == state.source_identity
        && prior.membership == state.membership
        && prior.inputs.as_ref() == Some(inputs)
        && prior.recipient == identity_recipient(paths, None)?
    {
        return Ok(Some(Arc::clone(prior)));
    }
    let Some(Selected {
        entries, recipient, ..
    }) = selected(paths, config, state)?
    else {
        return Ok(None);
    };
    let after = config
        .signer
        .is_some()
        .then(|| vault_memo::selection_inputs(paths))
        .flatten();
    if inputs
        .as_ref()
        .is_some_and(|before| after.as_ref() != Some(before))
    {
        return Ok(None);
    }
    let inputs = inputs.filter(|before| after.as_ref() == Some(before));
    Ok(Some(Arc::new(Fact {
        config: config.clone(),
        sequence: state.seq,
        envelope: state.digest,
        records: state.records_digest,
        source: state.source_identity,
        membership: state.membership,
        recipient,
        inputs,
        meaning: Meaning::of(&entries),
    })))
}
