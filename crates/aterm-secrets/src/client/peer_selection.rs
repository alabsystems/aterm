// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared peer admission for ordinary and deadline-bound readers.

use super::*;

/// Replacement resets the retired lineage's sequence before application.
pub(super) fn primary_advances(before: &State, envelope: &Envelope, pinned: Pin) -> bool {
    matches!(pinned, Pin::Replaced(_)) || envelope.seq > before.seq
}

/// Whether [`select`] admits a peer's head at exactly the version this Mac holds (`before`'s
/// seq and digest) beside the source's answer `primary`, `primary_is_held` saying the source's
/// own body verified as that very version: never then, since that body is the version and its
/// receipt; always where the pass does not repair its source (`repairs_primary`); otherwise where
/// the source failed, served a body that is no older version of this vault, or answered with no
/// such vault, an older version or a `204` while this Mac holds no verified copy of its own to
/// stand on or publish back. A pass that asks its peers only for newer versions (`?after`) hears
/// no such head, so it asks so only where this is false ([`super::sync_repairing_inner`]).
pub(super) fn admits_exact(
    paths: &Paths,
    config: &Config,
    before: &State,
    primary: &crate::Result<Fetched>,
    primary_is_held: bool,
    repairs_primary: bool,
) -> bool {
    if primary_is_held {
        // This response already supplies the exact body and its configured-source
        // receipt. An identical peer adds no recovery and must not replace it.
        false
    } else if !repairs_primary {
        true
    } else {
        match primary {
            Err(_) => true,
            Ok(Fetched::Missing) => publishable_copy(paths, config, before).is_none(),
            Ok(Fetched::Body(bytes)) => {
                // The served body may predate a signer rotation. As in apply,
                // its parsed sequence identifies the repair target; only our
                // verified held copy is eligible for publication back to it.
                !matches!(config.source, Source::Url(_))
                    || !envelope::parse(bytes)
                        .is_ok_and(|e| e.name == config.name && e.seq < before.seq)
                    || publishable_copy(paths, config, before).is_none()
            }
            // A 204 (or a held request with no body) cannot reconstruct a lost
            // cache. Keep normal no-change/older-source handling only while
            // the exact verified envelope is still available locally.
            Ok(Fetched::NoNewer(_)) => verified_cached_envelope(paths, config, before).is_none(),
        }
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerReplacement",
        action = "UsePrimary",
        reason = "Configured source supplies the replacement lineage directly"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerReplacement",
        action = "Retire",
        reason = "Compiled policy explicitly replaces a retired signer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerReplacement",
        action = "WrongKey",
        reason = "Environment supplies a peer outside the compiled replacement policy"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerReplacement",
        action = "Newer",
        reason = "Replacement lineage has an independent sequence number"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerReplacement",
        action = "LoseCache",
        reason = "Local damage removes the retired envelope body"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerReplacement",
        action = "Bounded",
        reason = "Caller chooses the bounded reader mode"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultPeerReplacement",
        action = "Select",
        project = "aterm_secrets::client::peer_selection_tests::replacement_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "NextLegacy",
        reason = "Environment supplies an unparented peer that preserves or drops held entries"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "HaveExact",
        reason = "Environment supplies a peer serving the exact held envelope"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "HaveGood",
        reason = "Environment supplies a signed applicable descendant in the peer batch"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "HaveFork",
        reason = "Environment supplies a higher signed non-descendant in the peer batch"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "NextSource",
        reason = "Environment chooses the configured source's response or transport failure"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "Bounded",
        reason = "Caller chooses the read-only bounded mode instead of ordinary repair"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "Revoke",
        reason = "Authority signs a descendant that intentionally removes the reader's locker"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultPeerSelection",
        action = "LoseCache",
        reason = "Local damage removes the verified envelope while its held version floor survives"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultPeerSelection",
        action = "Select",
        project = "aterm_secrets::client::peer_selection_tests::selection_transition"
    )
)]
pub(super) fn select(
    paths: &Paths,
    config: &Config,
    before: &State,
    primary: &crate::Result<Fetched>,
    found: Vec<SyncPeerHead>,
    repairs_primary: bool,
    deadline: Option<Instant>,
) -> crate::Result<Option<SyncPeerHead>> {
    let Some(pin) = config.signer.as_ref() else {
        return Ok(None);
    };
    if found.is_empty() {
        return Ok(None);
    }
    let verified_primary = match primary {
        Ok(Fetched::Body(bytes)) => {
            pass_counts::add(Count::FreshPrimary);
            envelope::verify_for_reader(bytes, &config.name, pin, &config.followed, None)
                .ok()
                .map(|(envelope, pinned, _)| (envelope, pinned))
        }
        _ => None,
    };
    // The configured source remains authoritative when it advances. Admins
    // have already had their opportunity to reconcile divergent server heads.
    if verified_primary
        .as_ref()
        .is_some_and(|(e, pinned)| primary_advances(before, e, *pinned))
    {
        return Ok(None);
    }
    let primary_is_held = verified_primary
        .as_ref()
        .is_some_and(|(e, _)| e.seq == before.seq && before.digest == Some(e.digest()));
    let allow_exact = admits_exact(
        paths,
        config,
        before,
        primary,
        primary_is_held,
        repairs_primary,
    );
    let held = floor(paths, before, &config.name, config);
    // Recover the exact body before comparing any advancing peer. An exact
    // peer is useful evidence even when ordinary source repair suppresses its
    // selection. Do not publish or cache it until the normal apply path runs.
    let is_held = |e: &Envelope| e.seq == before.seq && before.digest == Some(e.digest());
    // The held vault's entry answers for the bytes it was parsed from, and so for a peer's body
    // that is those very bytes; any other body is parsed and verified whole
    // ([`chain_cache::verify_as_held`]).
    let held_entry = super::chain_cache::held(&paths.cache_file()).ok().flatten();
    let peer_sites = (Count::MemoSelectPeer, Count::FreshSelectPeer);
    let reader = |floor| super::chain_cache::Reader {
        name: &config.name,
        pin,
        followed: &config.followed,
        floor,
    };
    // A changed URL does not invalidate signed comparison evidence. The
    // source-bound cache helpers still control reuse and publication elsewhere.
    let mut baseline = held_entry
        .as_deref()
        .filter(|parsed| {
            pass_counts::add(Count::MemoSelectHeld);
            parsed
                .verify_for_reader(&config.name, pin, &config.followed, held.as_ref())
                .is_ok_and(|(e, _, _)| is_held(e))
        })
        .map(|parsed| parsed.bytes().to_vec())
        .or_else(|| match primary {
            Ok(Fetched::Body(bytes)) if primary_is_held => {
                pass_counts::add(Count::FreshPrimary);
                envelope::verify_for_reader(
                    bytes,
                    &config.name,
                    pin,
                    &config.followed,
                    held.as_ref(),
                )
                .is_ok_and(|(e, _, _)| is_held(&e))
                .then(|| bytes.clone())
            }
            _ => None,
        });
    if baseline.is_none() {
        for peer in &found {
            bounded::budget(deadline, Duration::MAX)?;
            if before.digest == Some(peer.digest)
                && peer.seq == before.seq
                && super::chain_cache::verify_as_held(
                    held_entry.as_deref(),
                    &peer.envelope,
                    reader(held.as_ref()),
                    peer_sites,
                    |e, _| is_held(e),
                )
                .unwrap_or(false)
            {
                baseline = Some(peer.envelope.clone());
                break;
            }
        }
    }
    // Cache damage loses the handoff chain as well as entry coverage. Recover
    // both from the authenticated exact body before verifying newer heads.
    let recovered = baseline
        .as_ref()
        .and_then(|bytes| envelope::parse(bytes).ok())
        .map(|e| Floor::of(&e));
    let held = recovered.as_ref().or(held.as_ref());
    let mut admitted = Vec::new();
    for peer in found {
        bounded::budget(deadline, Duration::MAX)?;
        let Ok(peer_pin) = super::chain_cache::verify_as_held(
            held_entry.as_deref(),
            &peer.envelope,
            reader(held),
            peer_sites,
            |_, peer_pin| peer_pin,
        ) else {
            continue;
        };
        let exact = before.digest == Some(peer.digest) && peer.seq == before.seq;
        let bootstrap = before.digest.is_none()
            && peer.seq == before.seq
            && matches!(primary, Err(_) | Ok(Fetched::Missing));
        // Compiled replacement policy starts a new signer lineage. Its sequence
        // and contents need not subsume the retired lineage's held version.
        let eligible = matches!(peer_pin, Pin::Replaced(_))
            || ((exact && allow_exact) || bootstrap)
            || (peer.seq > before.seq
                && peer_subsumes_held(paths, config, before, &peer, baseline.as_deref()));
        if eligible {
            admitted.push(peer);
        }
    }
    Ok(admitted.into_iter().max_by_key(|p| (p.seq, p.digest)))
}

/// Called under the state lock. An unconditional repair may reuse its
/// authenticated cache when the endpoint has no body. Conditional polls stay
/// quiet; an older ETag retains put-back precedence only for a publishing reader.
/// Neither cache use supplies evidence of receiving an envelope over the network.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedMaterialization",
        action = "RequestFull",
        reason = "Caller chooses an unconditional refresh instead of a conditional poll"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedMaterialization",
        action = "Unsettle",
        reason = "A newer selection, or a failed apply, leaves the applied one not current"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedMaterialization",
        action = "LoseCache",
        reason = "Local damage makes the exact authenticated envelope unavailable"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedMaterialization",
        action = "Revoke",
        reason = "Prior membership application has removed this reader"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedMaterialization",
        action = "LegacyState",
        reason = "Persisted state predates recording what this reader reads"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedMaterialization",
        action = "Older",
        reason = "Server responds with an ETag older than the locally held envelope"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedMaterialization",
        action = "BoundedReader",
        reason = "A bounded reader requests local recovery without publishing to the source"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCachedMaterialization",
        action = "Decide",
        project = "aterm_secrets::client::peer_selection_tests::cached_materialization_transition"
    )
)]
pub(super) fn cached_materialization(
    paths: &Paths,
    config: &Config,
    before: &State,
    unconditional: bool,
    repairs_primary: bool,
    served: Option<u64>,
    body: &mut Option<network_observation::Body>,
) -> Option<Vec<u8>> {
    if unconditional
        && (!repairs_primary || served.is_none_or(|seq| seq >= before.seq))
        && before.membership == Stored::In
        && source_bound(config, before)
        && (!applied_intact(paths, before) || before.held.is_none())
        && let Some(copy) = verified_cached_envelope(paths, config, before)
    {
        *body = None;
        Some(copy)
    } else {
        None
    }
}

/// Select only a present, authenticated local log change for replay.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "ChangeLog",
        reason = "A verified local record set changes before selection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "LoseLog",
        reason = "The local records become unavailable"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "LoseCache",
        reason = "The exact authenticated envelope cache becomes unavailable"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "NoAppliedLog",
        reason = "A reader has not yet applied a local record set"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "ReplaceLineage",
        reason = "A compiled signer replacement resets the prior log binding"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "BehindStamp",
        reason = "Available log records do not cover the held envelope stamp"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "Pending",
        reason = "The reader is not admitted by the available records"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultCachedLogReplay",
        action = "Admit",
        reason = "An authorized admission and wrap reach the local record set"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCachedLogReplay",
        action = "Select",
        project = "aterm_secrets::client::peer_selection_tests::cached_log_transition"
    )
)]
pub(super) fn cached_log_progress(
    paths: &Paths,
    config: &Config,
    state: &State,
    body: &mut Option<network_observation::Body>,
) -> Option<Vec<u8>> {
    let copy = verified_cached_envelope(paths, config, state)?;
    let envelope = envelope::parse(&copy).ok()?;
    let (log, digest) = log_entries::trusted_seeded_log(paths, config, &envelope)
        .ok()
        .flatten()?;
    if state.records_digest == Some(digest) && materialization_selection_current(paths, state) {
        return None;
    }
    let identity = read_identity(paths).ok().flatten()?;
    if log_entries::log_base(Some(&log), &envelope, &identity).ok()? == log_entries::LogBase::Behind
    {
        return None;
    }
    if state.membership != Stored::In
        && log_entries::open_entries_with_log(Some((&log, digest)), &envelope, &identity, None)
            .ok()?
            .0
            .is_none()
    {
        return None;
    }
    *body = None;
    Some(copy)
}

/// [`cached_log_progress`] at most once an apply. Each place an apply falls back to the cache's
/// own log progress asks it of the same cache, log and state (the apply moves only `heard`
/// between them), and each ask verified the cache and resolved this Mac's entries again
/// (ATERM-KI-1113-902). A network body is let go wherever the progress is taken, as each ask
/// did.
#[derive(Default)]
pub(super) struct LogProgress(std::cell::OnceCell<Option<Vec<u8>>>);

impl LogProgress {
    pub(super) fn of(
        &self,
        paths: &Paths,
        config: &Config,
        state: &State,
        body: &mut Option<network_observation::Body>,
    ) -> Option<Vec<u8>> {
        let copy = self
            .0
            .get_or_init(|| {
                #[cfg(test)]
                LOG_PROGRESS_ASKS.with(|asks| asks.set(asks.get() + 1));
                cached_log_progress(paths, config, state, &mut None)
            })
            .clone();
        if copy.is_some() {
            *body = None;
        }
        copy
    }
}

#[cfg(test)]
thread_local! {
    /// Each [`cached_log_progress`] a [`LogProgress`] asked on this thread.
    pub(super) static LOG_PROGRESS_ASKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// An envelope cannot replace values already materialized from an unavailable log.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCachedLogReplay",
        action = "Guard",
        project = "aterm_secrets::client::peer_selection_tests::cached_log_transition"
    )
)]
pub(super) fn awaiting_applied_log(state: &State, records_digest: Option<[u8; 32]>) -> bool {
    state.records_digest.is_some() && records_digest.is_none()
}

/// A bound log-only member can apply received records or repair files without an envelope.
/// Called under the state lock; an unrelated nonmember update leaves network recovery due.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "HaveCache",
        reason = "Environment supplies an envelope file that needs its own verification path"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "Envelope",
        reason = "Environment chooses a materialized envelope instead of log-only state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "Unbind",
        reason = "Environment changes the source binding"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "LoseLog",
        reason = "Local damage makes the authenticated log unavailable"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "Replace",
        reason = "Compiled policy replaces the configured signer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "NeedWork",
        reason = "Environment supplies received records or incomplete materialization"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "Pending",
        reason = "Environment supplies a member without a readable admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VaultConfiguredLogReplay",
        action = "Admit",
        reason = "Authenticated records supply a readable admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultConfiguredLogReplay",
        action = "Select",
        project = "aterm_secrets::client::log_only_tests::configured_log_transition"
    )
)]
pub(super) fn configured_log_recovery(
    paths: &Paths,
    config: &Config,
    state: &State,
) -> Option<log_entries::ConfiguredLog> {
    if state.seq != 0
        || !log_only_source_bound(config, state)
        || pin_gives_way(config)
        || fsx::kind(&paths.cache_file()).ok() != Some(fsx::Kind::Missing)
    {
        return None;
    }
    let configured = log_entries::trusted_configured_log(paths, config)
        .ok()
        .flatten()?;
    let identity = read_identity(paths).ok().flatten()?;
    if state.membership != Stored::In
        && log_entries::configured_entries(&configured.log, &identity)
            .ok()
            .flatten()
            .is_none()
    {
        return None;
    }
    if state.records_digest == Some(log_entries::records_heads_digest(&configured.log))
        && state.held.is_some()
        && applied_intact(paths, state)
    {
        return None;
    }
    Some(configured)
}
