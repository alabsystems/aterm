// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public ALab join requests reach verified mirrors as well as the configured queue.
//! A mirror can hold the request while an approver reads another copy of the vault.
//! Password proofs and signed member Allows keep their single configured destination.

use super::*;

#[cfg(not(test))]
const MIRROR_WITHIN: Duration = PROBE_DEADLINE.saturating_mul(2);
#[cfg(test)]
const MIRROR_WITHIN: Duration = Duration::from_secs(60);

fn public_alab_request(config: &Config, line: &str) -> bool {
    alab_vault(config) && public_request(line)
}

fn public_request(line: &str) -> bool {
    transport::parse_join_line(line)
        .is_ok_and(|request| request.proof.is_none() && request.allowed.is_none())
}

/// Post through the configured queue and, for public ALab requests, verified mirrors. The
/// member's automatic forwarding desk uses the same route as this machine's own join.
pub(crate) fn post(
    paths: &Paths,
    config: &Config,
    line: &str,
    deadline: Option<Instant>,
) -> crate::Result<()> {
    let Source::Url(primary) = &config.source else {
        return Ok(());
    };
    if !public_alab_request(config, line) {
        return post_primary(primary, line, deadline);
    }
    post_discovering(config, line, deadline, |until| {
        targets(paths, config, primary, &PeerDiscovery::direct(Some(until)))
    })
}

/// Join attempts advance independently of vault reads, including across process restarts.
pub(super) fn targets(
    paths: &Paths,
    config: &Config,
    primary: &str,
    found: &PeerDiscovery<'_>,
) -> Vec<String> {
    collect_sync_peer_urls(paths, config, primary, &[], Some(found), PeerUse::Join)
}

fn post_primary(url: &str, line: &str, deadline: Option<Instant>) -> crate::Result<()> {
    match deadline {
        Some(until) => transport::post_join_until(url, line, until),
        None => transport::post_join(url, line),
    }
}

/// The configured request and discovery run together. Extra destinations share one bounded
/// budget, and receive only this public request after proving the same vault's signing lineage.
/// A failed configured queue is tolerable only when a verified peer actually accepted it.
pub(super) fn post_discovering(
    config: &Config,
    line: &str,
    deadline: Option<Instant>,
    discover: impl FnOnce(Instant) -> Vec<String>,
) -> crate::Result<()> {
    let Source::Url(primary) = &config.source else {
        return Ok(());
    };
    if !public_request(line) {
        return post_primary(primary, line, deadline);
    }
    let until = deadline
        .unwrap_or_else(|| Instant::now() + MIRROR_WITHIN)
        .min(Instant::now() + MIRROR_WITHIN);
    std::thread::scope(|scope| {
        let configured = scope.spawn(|| post_primary(primary, line, deadline));
        let peers = discover(until);
        let mut seen = BTreeSet::new();
        let attempts: Vec<_> = peers
            .into_iter()
            .filter(|url| !url.eq_ignore_ascii_case(primary) && seen.insert(url.clone()))
            .take(crate::discover::MAX_CANDIDATES)
            .map(|url| scope.spawn(move || post_verified(config, &url, line, until)))
            .collect();
        let mut accepted = false;
        for attempt in attempts {
            accepted |= attempt.join().is_ok_and(|result| result.is_ok());
        }
        let configured = configured
            .join()
            .unwrap_or_else(|_| Err(Error::new("configured join request panicked")));
        if accepted { Ok(()) } else { configured }
    })
}

fn post_verified(config: &Config, url: &str, line: &str, until: Instant) -> crate::Result<()> {
    verify_mirror(config, url, until)?;
    transport::post_join_until(url, line, until)
}

/// Verify the mirror's vault before sending a public request or trusting a policy server's
/// queue. The caller supplies one deadline across verification and its subsequent operation.
pub(super) fn verify_mirror(config: &Config, url: &str, until: Instant) -> crate::Result<()> {
    let remaining = until
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| {
            Error::of_kind(crate::ErrorKind::TimedOut, "join mirror deadline elapsed")
        })?;
    let pin = config
        .signer
        .ok_or_else(|| Error::new("join mirror needs a signing pin"))?;
    let body = match transport::get_within(url, remaining)? {
        transport::Answer::Fetched(Fetched::Body(body)) => body,
        _ => return Err(Error::new("join mirror did not supply a vault")),
    };
    envelope::verify_for_reader(&body, &config.name, &pin, &config.followed, None)?;
    Ok(())
}

#[cfg(test)]
#[path = "join_routing_tests.rs"]
mod tests;
