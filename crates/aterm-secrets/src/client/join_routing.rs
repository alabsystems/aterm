// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public ALab join requests reach verified mirrors as well as the configured queue.
//! A mirror can hold the request while an approver reads another copy of the vault.
//! Password proofs and signed member Allows keep their single configured destination.

use super::*;

#[cfg(not(test))]
const MIRROR_WITHIN: Duration = PROBE_DEADLINE.saturating_mul(2);
/// Past the hang detector, so a test that counts the mirrors' reads ([`fan_out::held`]) neither
/// ends one at the deadline nor sees the join finish before it has counted.
#[cfg(test)]
const MIRROR_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN.saturating_mul(2);

fn public_alab_request(config: &Config, line: &str) -> bool {
    alab_vault(config) && public_request(line)
}

fn public_request(line: &str) -> bool {
    transport::parse_join_line(line)
        .is_ok_and(|request| request.proof.is_none() && request.allowed.is_none())
}

/// Post through the configured queue and, for public ALab requests, verified mirrors.
pub(super) fn post(
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
/// budget, and receive only this public request after proving the same vault's signing lineage
/// ([`post_verified`]). A failed configured queue is tolerable only when a verified peer actually
/// accepted it.
pub(super) fn post_discovering(
    config: &Config,
    line: &str,
    deadline: Option<Instant>,
    discover: impl FnOnce(Instant) -> Vec<String>,
) -> crate::Result<()> {
    post_discovering_through(
        config,
        line,
        deadline,
        discover,
        transport::look_within,
        transport::post_join_until,
    )
}

/// [`post_discovering`], each mirror's vault read by `look` ([`transport::look_within`]) and the
/// request posted to a mirror whose vault verified by `post` ([`transport::post_join_until`]).
fn post_discovering_through(
    config: &Config,
    line: &str,
    deadline: Option<Instant>,
    discover: impl FnOnce(Instant) -> Vec<String>,
    look: impl Fn(&str, Duration, usize) -> Looked + Sync,
    post: impl Fn(&str, &str, Instant) -> crate::Result<()> + Sync,
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
        let mirrors: Vec<String> = peers
            .into_iter()
            .filter(|url| !url.eq_ignore_ascii_case(primary) && seen.insert(url.clone()))
            .take(crate::discover::MAX_CANDIDATES)
            .collect();
        let accepted = post_verified(config, &mirrors, line, until, look, post);
        let configured = configured
            .join()
            .unwrap_or_else(|_| Err(Error::new("configured join request panicked")));
        if accepted { Ok(()) } else { configured }
    })
}

/// `post` of `line` to each of `mirrors` whose vault verifies as `config`'s, all of them at once,
/// each mirror's read and post ending by `until`: whether any of them accepted it. The mirrors'
/// vaults are read by `look` under [`fan_out::reads_within`]'s bound ([`fan_out::ENVELOPES`]):
/// every answer under the peek at once, and one longer than that whole under one of a few slots
/// (the first naming of a compiled-in default address under none), each slot held until that
/// mirror's vault was judged and, where it verified, the request posted to it. The mirrors are
/// the join's peer plan ([`targets`]: the compiled-in defaults, the signed swarm policy's servers
/// and those a browse named), and up to [`crate::discover::MAX_CANDIDATES`] of them can be a
/// browse's, which any host on the LAN can advertise and nothing has verified: read whole at
/// once, each on a thread of its own, 16 instances of one host, each answering with a 16 MiB
/// envelope, had a join hold 256 MiB of them before any was refused. A mirror whose vault does
/// not verify receives nothing, and without a pinned signer no mirror is read.
fn post_verified(
    config: &Config,
    mirrors: &[String],
    line: &str,
    until: Instant,
    look: impl Fn(&str, Duration, usize) -> Looked + Sync,
    post: impl Fn(&str, &str, Instant) -> crate::Result<()> + Sync,
) -> bool {
    let Some(pin) = config.signer else {
        return false;
    };
    fan_out::reads_within(
        fan_out::ENVELOPES,
        mirrors,
        until.saturating_duration_since(Instant::now()),
        look,
        |url, read| {
            let answer = match read? {
                transport::Pulled::Fetched(answer) => answer,
                // What one look of the whole answer ([`transport::get_within`]) makes of it.
                transport::Pulled::Larger => {
                    transport::Answer::Foreign(transport::past_any_envelope(url))
                }
            };
            verified(config, &pin, answer)?;
            post(url, line, until)
        },
    )
    .into_iter()
    .any(|posted| matches!(posted, Some(Ok(()))))
}

/// Verify the mirror's vault before trusting a policy server's queue (`join_queues`). The caller
/// supplies one deadline across verification and its subsequent operation.
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
    verified(config, &pin, transport::get_within(url, remaining)?)
}

/// That a mirror's `answer` is a vault that verifies as `config`'s under `pin`, or why it is not.
fn verified(config: &Config, pin: &[u8; 32], answer: transport::Answer) -> crate::Result<()> {
    let body = match answer {
        transport::Answer::Fetched(Fetched::Body(body)) => body,
        _ => return Err(Error::new("join mirror did not supply a vault")),
    };
    envelope::verify_for_reader(&body, &config.name, pin, &config.followed, None)?;
    Ok(())
}

#[cfg(test)]
#[path = "join_routing_tests.rs"]
mod tests;
