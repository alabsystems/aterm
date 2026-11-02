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

/// Fetch verified membership records and the best available envelope, then materialize them.
/// Discovery, network requests and state-lock waits share one absolute `deadline`. This member
/// pass does not mirror, repost records or put an envelope back on a server; the standing
/// subscriber owns that upkeep. It uses the same admission, pin, floor and materialization
/// checks as [`sync`].
///
/// # Errors
/// Configuration, verification or materialization fails, no source answers, or the deadline
/// elapses before a required request or state lock completes.
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
        let _ = sync_records_until(paths, None, &[], Some(&found), Some(deadline));
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
                budget(Some(deadline), PROBE_DEADLINE)?,
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
                    .is_ok_and(|(envelope, _, _)| envelope.seq > before.seq)
            }
            _ => false,
        };
        if !primary_advanced && let Some(pin) = config.signer.as_ref() {
            let found: Vec<SyncPeerHead> = std::thread::scope(|scope| {
                let handles: Vec<_> = peers
                    .iter()
                    .map(|url| {
                        let config = &config;
                        let subscriber = subscriber.as_deref();
                        scope.spawn(move || {
                            let left = budget(Some(deadline), PROBE_DEADLINE).ok()?;
                            probe_sync_peer(paths, url, config, pin, left, subscriber)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .filter_map(|h| h.join().ok().flatten())
                    .collect()
            });
            let exact = before.digest.and_then(|digest| {
                found
                    .iter()
                    .find(|p| p.seq == before.seq && p.digest == digest)
                    .map(|p| (p.envelope.clone(), p.network_body))
            });
            if let Some(peer) = found.into_iter().max_by_key(|p| (p.seq, p.digest)) {
                let take_peer = match &fetched {
                    Err(_) => {
                        peer.seq > before.seq
                            || (peer.seq == before.seq
                                && before.digest.is_none_or(|digest| digest == peer.digest))
                    }
                    Ok(Fetched::Missing) => peer.seq > before.seq || before.seq == 0,
                    Ok(Fetched::NoNewer(_) | Fetched::Body(_)) => {
                        peer.seq > before.seq && peer_subsumes_held(paths, &config, &before, &peer)
                    }
                };
                if take_peer {
                    network_body = peer.network_body;
                    fetched = Ok(Fetched::Body(peer.envelope));
                } else if let Some((exact, exact_body)) = exact {
                    // The exact head already held is also safe when the primary is missing,
                    // stale or invalid. Reapplying it lets newly fetched membership records
                    // take effect without requiring this reader to heal the primary first.
                    network_body = exact_body;
                    fetched = Ok(Fetched::Body(exact));
                }
            }
        }
        if network_body.is_some() {
            refresh.peer_response();
        }
        let fetched = fetched.or_else(|error| {
            if log_entries::current_records_digest(paths, &config, &before) != before.records_digest
                && let Some(copy) = verified_cached_envelope(paths, &config, &before)
            {
                network_body = None;
                Ok(Fetched::Body(copy))
            } else {
                Err(error)
            }
        })?;
        apply_fetched_until(
            paths,
            config.clone(),
            before,
            network_observation::Received {
                fetched,
                body: network_body,
            },
            None,
            Some(deadline),
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
    let urls =
        crate::discover::candidate_urls(crate::ALAB_DEFAULT_URLS, &discovered, crate::ALAB_VAULT);
    let outcomes: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = urls
            .iter()
            .map(|url| {
                scope.spawn(move || {
                    budget(Some(deadline), PROBE_DEADLINE).and_then(|left| probe(url, &pin, left))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(Error::new("vault probe thread panicked")))
            })
            .collect()
    });
    select_probed_outcome(urls.iter().zip(outcomes).map(|(url, outcome)| {
        let origin = if crate::is_alab_default_url(url) {
            ProbeOrigin::Default
        } else {
            ProbeOrigin::Discovered
        };
        (origin, outcome)
    }))
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
            let left = budget(Some(deadline), PROBE_DEADLINE)?;
            match source {
                Source::Url(url) => transport::fetch_within(url, None, left),
                Source::Path(_) => transport::fetch(source, None, Duration::ZERO),
            }
        },
        Some(deadline),
    )
}
