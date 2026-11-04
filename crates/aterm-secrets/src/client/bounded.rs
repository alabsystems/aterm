// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bound member admission and reads for the automatic Commons recovery pass.

use super::*;

pub(super) fn budget(deadline: Option<Instant>, maximum: Duration) -> crate::Result<Duration> {
    match deadline {
        Some(deadline) => deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .map(|left| left.min(maximum))
            .ok_or_else(|| {
                Error::of_kind(crate::ErrorKind::TimedOut, "member sync deadline elapsed")
            }),
        None => Ok(maximum),
    }
}

pub(super) fn take(paths: &Paths, deadline: Option<Instant>) -> crate::Result<StateLock> {
    match deadline {
        Some(deadline) => {
            budget(Some(deadline), Duration::MAX)?;
            StateLock::try_take_until(paths, deadline)?.ok_or_else(|| {
                Error::of_kind(crate::ErrorKind::TimedOut, "member state is still locked")
            })
        }
        None => StateLock::take(paths),
    }
}

/// Apply already-received log changes or repair local files from the authenticated cache if
/// possible; otherwise fetch verified membership records and the best available envelope,
/// then materialize them.
/// Discovery, network requests and state-lock waits share one absolute `deadline`. This member
/// pass does not mirror, repost records or put an envelope back on a server; the standing
/// subscriber owns that upkeep. It uses the same admission, pin, floor and materialization
/// checks as [`sync`].
///
/// # Errors
/// Configuration, verification or materialization fails, neither a source nor an admitted
/// cache supplies recovery, or the deadline elapses before a required request or state lock
/// completes.
pub fn sync_reader_until(paths: &Paths, deadline: Instant) -> crate::Result<SyncOutcome> {
    sync_reader_with_candidates_until(paths, deadline, &[])
}

pub(super) fn sync_reader_with_candidates_until(
    paths: &Paths,
    deadline: Instant,
    candidates: &[String],
) -> crate::Result<SyncOutcome> {
    budget(Some(deadline), Duration::MAX)?;
    let config = configured(paths)?;
    let before = load_state(paths)?;
    // Network discovery and stalled peers can consume the whole deadline.
    // Local recovery gets its chance first, under that same hard deadline,
    // without inventing a network attempt or renewing its receipts.
    let local = apply_until(
        paths,
        config.clone(),
        before.clone(),
        ApplyInput::Cached,
        None,
        Some(deadline),
        true,
    )?;
    match local {
        SyncOutcome::Unchanged { .. } => {}
        SyncOutcome::Applied { ref report, .. } if apply_failed(report) => {}
        complete => return Ok(complete),
    }
    let config = configured(paths)?;
    let before = load_state(paths)?;
    let subscriber = subscriber_recipient(paths);
    let peers = if let Source::Url(primary) = &config.source {
        budget(Some(deadline), PROBE_DEADLINE)?;
        let found = PeerDiscovery::direct(Some(deadline));
        collect_sync_peer_urls(
            paths,
            &config,
            primary,
            candidates,
            Some(&found),
            PeerUse::Reader,
        )
    } else {
        Vec::new()
    };
    if matches!(config.source, Source::Url(_)) {
        let found = PeerDiscovery::selected(&peers);
        // A failed record server may still serve an envelope. Keep the ordinary reader's
        // independent pulls, but every subsequent operation still uses this same deadline.
        let _ = sync_records_until(paths, None, &[], Some(&found), Some(deadline), None);
    }
    network_refresh::run(paths, &config, |refresh| {
        let attempt =
            network_observation::Attempt::start(paths, &config, VaultNetworkSource::ConfiguredUrl);
        let http204_request =
            http204_observation::Request::start(paths, &config, None, subscriber.as_deref());
        let observed = match &config.source {
            Source::Url(url) => transport::fetch_within_as_observed(
                url,
                None,
                budget(Some(deadline), servers_within(PROBE_DEADLINE))?,
                subscriber.as_deref(),
            ),
            Source::Path(_) => {
                budget(Some(deadline), Duration::MAX)?;
                transport::fetch_as_observed(
                    &config.source,
                    None,
                    Duration::ZERO,
                    subscriber.as_deref(),
                )
            }
        };
        refresh.observed(&observed);
        if let (Some(request), Ok(observed)) = (&http204_request, &observed) {
            http204_observation::retain(paths, &config, request, observed.http204.as_ref());
        }
        let mut fetched = observed.map(|observed| observed.fetched);
        let mut network_body = fetched
            .as_ref()
            .ok()
            .and_then(|fetched| attempt.and_then(|attempt| attempt.received(fetched)));
        let primary_advanced = match (&fetched, config.signer.as_ref()) {
            (Ok(Fetched::Body(bytes)), Some(pin)) => {
                envelope::verify_for_reader(bytes, &config.name, pin, &config.followed, None)
                    .is_ok_and(|(envelope, pinned, _)| {
                        peer_selection::primary_advances(&before, &envelope, pinned)
                    })
            }
            _ => false,
        };
        if !primary_advanced && let Some(pin) = config.signer.as_ref() {
            let peers_within = budget(Some(deadline), servers_within(PROBE_DEADLINE));
            let found: Vec<SyncPeerHead> = match peers_within {
                Ok(left) => {
                    probe_peers_for_sync(paths, &peers, &config, pin, left, subscriber.as_deref())
                }
                Err(_) => Vec::new(),
            };
            if let Some(peer) = peer_selection::select(
                paths,
                &config,
                &before,
                &fetched,
                found,
                false,
                Some(deadline),
            )? {
                network_body = peer.network_body;
                fetched = Ok(Fetched::Body(peer.envelope));
            }
        }
        if network_body.is_some() {
            refresh.peer_response();
        }
        apply_response_until(
            paths,
            config.clone(),
            before,
            fetched.map(|fetched| network_observation::Received {
                fetched,
                body: network_body,
            }),
            None,
            Some(deadline),
            true,
        )
    })
}

fn probe_default_until(deadline: Instant) -> crate::Result<ProbeOutcome> {
    let Some(pin) = decode_key32(crate::ALAB_SIGNER) else {
        return Ok(ProbeOutcome::Absent);
    };
    let discovered = match crate::discover::browse_alab(budget(Some(deadline), PROBE_DEADLINE)?) {
        Ok(discovered) => discovered,
        Err(error) if refused_by_local_network(&error) => return Err(error),
        Err(_) => Vec::new(),
    };
    let left = budget(Some(deadline), PROBE_DEADLINE)?;
    probe_alab(&pin, &discovered, left, transport::look_within)
}

/// Join over exactly `expected`, sharing `deadline` across discovery, fetch, materialization,
/// state locks and the join POST (including any Retry-After). This unattended entry point
/// requires noninteractive options, no password and zero approval wait. Notification callbacks
/// must return promptly. Admission still requires the vault's ordinary approval policy.
///
/// # Errors
/// Options require interaction, the configuration changed, ordinary join checks fail, or the
/// deadline elapses before a required request or state lock completes.
pub fn join_if_unchanged_until(
    paths: &Paths,
    options: &JoinOptions<'_>,
    expected: Option<&Config>,
    deadline: Instant,
) -> crate::Result<JoinOutcome> {
    if options.interactive
        || !options.wait.is_zero()
        || !matches!(options.password, JoinPassword::Given(None))
    {
        return Err(Error::new(
            "bounded join requires unattended options without a password or wait",
        ));
    }
    budget(Some(deadline), Duration::MAX)?;
    let typed = options.source.trim();
    let probed = if options.fetched.is_none()
        && alab_signer_built_in()
        && (typed.is_empty() || crate::is_alab_default_url(typed))
    {
        match probe_default_until(deadline) {
            Ok(ProbeOutcome::Found(probed)) => Some(probed),
            Err(error) if refused_by_local_network(&error) => return Err(error),
            _ => None,
        }
    } else {
        None
    };
    let resolved = probed.as_ref().map(|probed| JoinOptions {
        source: if crate::is_alab_default_url(&probed.url) {
            probed.url.clone()
        } else {
            format!("{}#{}", probed.url, crate::ALAB_SIGNER)
        },
        host: options.host.clone(),
        interactive: false,
        wait: Duration::ZERO,
        confirm: options.confirm,
        password: JoinPassword::Given(None),
        fetched: Some(probed.envelope.as_slice()),
        requested: options.requested,
        say: options.say,
    });
    join_with_until(
        paths,
        resolved.as_ref().unwrap_or(options),
        ExpectedConfig::Read(expected),
        |source| {
            let left = budget(Some(deadline), servers_within(PROBE_DEADLINE))?;
            match source {
                Source::Url(url) => transport::fetch_within(url, None, left),
                Source::Path(_) => transport::fetch(source, None, Duration::ZERO),
            }
        },
        Some(deadline),
    )
}
