// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read independent queues fairly, keeping each listing with its address and version.
//! Mirrors need authenticated policy authority and the vault's signing lineage.

use super::*;

#[cfg(not(test))]
const QUEUES_WITHIN: Duration = PROBE_DEADLINE.saturating_mul(2);
#[cfg(test)]
const QUEUES_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;

/// Independent observations, never a synthetic queue with a combined home-LAN claim.
pub(super) struct Observations {
    pub(super) queues: Vec<Heard>,
    pub(super) incomplete: bool,
}

/// Look for explicit identities across every authorized queue. Unlike the standing
/// approver's fair reader, this neither advances nor limits itself to a cursor batch.
/// Every connection shares the caller's deadline and the fan-out's thread bound.
pub(super) fn lookup(
    config: &Config,
    policy: &[String],
    deadline: Instant,
) -> crate::Result<Observations> {
    let Source::Url(primary) = &config.source else {
        return Ok(Observations {
            queues: Vec::new(),
            incomplete: false,
        });
    };
    let until = deadline.min(Instant::now() + servers_within(QUEUES_WITHIN));
    let urls = peer_candidates::policy_queues(config, primary, policy);
    let answers = fan_out::each_at_once("join-lookup", &urls, |url| {
        if url != primary {
            join_routing::verify_mirror(config, url, until)?;
        }
        read(url, &Heard::default(), Duration::ZERO, Some(until)).map(|(queue, _)| queue)
    });
    let mut queues = Vec::new();
    let mut incomplete = false;
    for answer in answers {
        match answer {
            Some(Ok(queue)) => queues.push(queue),
            Some(Err(_)) | None => incomplete = true,
        }
    }
    if queues.is_empty() {
        return Err(Error::of_kind(
            crate::ErrorKind::JoinQueueIncomplete,
            "no verified join queue answered",
        ));
    }
    Ok(Observations { queues, incomplete })
}

pub(super) fn hear(
    paths: &Paths,
    config: &Config,
    heard: &mut Heard,
    wait: Duration,
) -> crate::Result<(Vec<String>, bool)> {
    hear_bounded(paths, config, heard, wait, None)
}

pub(super) fn hear_until(
    paths: &Paths,
    config: &Config,
    heard: &mut Heard,
    deadline: Instant,
) -> crate::Result<(Vec<String>, bool)> {
    hear_bounded(paths, config, heard, Duration::ZERO, Some(deadline))
}

fn hear_bounded(
    paths: &Paths,
    config: &Config,
    heard: &mut Heard,
    wait: Duration,
    deadline: Option<Instant>,
) -> crate::Result<(Vec<String>, bool)> {
    if deadline.is_some_and(|end| Instant::now() >= end) {
        return Err(Error::of_kind(
            crate::ErrorKind::TimedOut,
            "join queue observation deadline elapsed",
        ));
    }
    let Source::Url(primary) = &config.source else {
        *heard = Heard::default();
        return Ok((Vec::new(), true));
    };
    let policy = if config.signer.is_some() {
        policy_peer_urls(paths, config)
    } else {
        Vec::new()
    };
    let ordinary = Instant::now() + servers_within(QUEUES_WITHIN);
    let until = deadline.map_or(ordinary, |end| end.min(ordinary));
    let discovery = PeerDiscovery::direct(Some(until));
    // The explicit pool excludes discovery hints: a queue an approver judges requests from
    // needs policy authority, as well as a mirror that proves the vault's signing lineage.
    let urls = if policy.is_empty() {
        Vec::new()
    } else {
        discovery.select_queues(paths, config, primary, &policy, &heard.url)
    };
    if !urls.iter().any(|url| url != primary) {
        return match read(primary, heard, wait, deadline.map(|_| until)) {
            Ok((current, moved)) => {
                let lines = current.lines.clone();
                *heard = current;
                Ok((lines, moved))
            }
            Err(error) => {
                heard.version = None;
                Err(error)
            }
        };
    }
    let held: &Heard = heard;
    // Poll the primary and the bounded mirror set together. An empty or nonempty
    // primary must not hide another queue, and a stalled source gets no extra budget.
    let answers = std::thread::scope(|scope| {
        let attempts: Vec<_> = urls
            .into_iter()
            .map(|url| {
                scope.spawn(move || {
                    let answer = if url == *primary {
                        Ok(())
                    } else {
                        join_routing::verify_mirror(config, &url, until)
                    }
                    .and_then(|()| read(&url, held, Duration::ZERO, Some(until)));
                    (url, answer)
                })
            })
            .collect();
        attempts
            .into_iter()
            .filter_map(|attempt| attempt.join().ok())
            .collect::<Vec<_>>()
    });
    let mut primary_error = None;
    let mut listed = Vec::new();
    for (url, answer) in answers {
        match answer {
            Ok(current) => listed.push(current),
            Err(error) if url == *primary => primary_error = Some(error),
            Err(_) => {}
        }
    }
    let chosen = listed
        .iter()
        .position(|(queue, _)| !queue.lines.is_empty())
        .or_else(|| listed.iter().position(|(queue, _)| queue.url == heard.url))
        .or_else(|| (!listed.is_empty()).then_some(0));
    let Some(chosen) = chosen else {
        heard.version = None;
        return Err(primary_error.unwrap_or_else(|| Error::new("no verified join queue answered")));
    };
    let (current, moved) = listed.swap_remove(chosen);
    let lines = current.lines.clone();
    *heard = current;
    Ok((lines, moved))
}

fn read(
    url: &str,
    heard: &Heard,
    wait: Duration,
    until: Option<Instant>,
) -> crate::Result<(Heard, bool)> {
    let after = heard.version.filter(|_| heard.url == url);
    let (answer, from) = match until {
        Some(until) => transport::wait_joins_from_until(url, after, until)?,
        None => transport::wait_joins_from(url, after, wait)?,
    };
    match answer {
        transport::Joins::Listed(lines, version) => Ok((
            Heard {
                url: url.to_owned(),
                version,
                lines,
                from,
            },
            true,
        )),
        transport::Joins::Unchanged
            if after.is_some() && heard.url == url && heard.from == from =>
        {
            Ok((heard.clone(), false))
        }
        // The URL now resolves to another server. Its version cannot validate the previous
        // server's listing; get a full listing within the same deadline.
        transport::Joins::Unchanged if after.is_some() => {
            read(url, &Heard::default(), Duration::ZERO, until)
        }
        transport::Joins::Unchanged => Err(Error::new("join queue returned no initial listing")),
    }
}
