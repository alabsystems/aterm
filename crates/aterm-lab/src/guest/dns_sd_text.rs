// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a `dns-sd` resolve's transcript says, read the same on this Mac (`lab mac nux`'s
//! `advert` module) and in the guest (`aterm-lab-guest dns-sd`): the hosts a `-L` names, the
//! addresses a `-G` names, and whether either has answered yet ([`answered`]).
//!
//! A resolve of an advert that was heard must answer, so a runner listens until it does, bounded
//! by its one-minute hang detector, rather than for a fixed window. The fixed two seconds ended
//! one resolve of this Mac's own vault advert over tart's bridge before its answer came, in E3's
//! vault row (2026-11-05, m22 at load average 9), and the advert read as a guest's.

use std::net::IpAddr;

/// The hosts a `dns-sd -L` transcript says its instance can be reached at
/// (`… can be reached at alab-secrets.local.:7484 (interface 25)`): lowercase, without the
/// trailing dot, each once. A host with other characters than a DNS name's is left out.
#[must_use]
pub fn reach_hosts(transcript: &str) -> Vec<String> {
    const REACHED: &str = " can be reached at ";
    let mut out: Vec<String> = Vec::new();
    for line in transcript.lines() {
        let Some((_, rest)) = line.split_once(REACHED) else {
            continue;
        };
        let target = rest.split(" (interface ").next().unwrap_or(rest).trim();
        let Some((host, _port)) = target.rsplit_once(':') else {
            continue;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let named = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
        if named && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// The fields of a `dns-sd -G` answer row (`Timestamp A/R Flags IF Hostname Address TTL`). A row
/// with more carries `dns-sd`'s word on an error (`No Such Record`, `Error code N`).
const ANSWER_FIELDS: usize = 7;

/// The addresses the `Add` rows of a `dns-sd -G v4v6 <host>` transcript name, each once: an IPv4
/// one as written, an IPv6 one without its scope (`FE80:…:C0EA%en0`, `…%<0>`). A negative answer
/// (`0.0.0.0 108 No Such Record`: mDNSResponder's word that the host has no address of that
/// family there), an error's row and an unspecified address name none.
#[must_use]
pub fn addresses(transcript: &str) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for line in transcript.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.get(1) != Some(&"Add") || f.len() != ANSWER_FIELDS {
            continue;
        }
        let Some(address) = f
            .get(5)
            .and_then(|a| a.split('%').next())
            .and_then(|a| a.parse::<IpAddr>().ok())
        else {
            continue;
        };
        if !address.is_unspecified() && !out.contains(&address) {
            out.push(address);
        }
    }
    out
}

/// Whether the transcript of `dns-sd ARGS...` holds the answer its run waits for: a `-L` a host
/// it can be reached at ([`reach_hosts`]), a `-G` an address ([`addresses`]; a negative answer is
/// none). Any other run (a `-B` browse) listens for its window alone: what it should hear is not
/// known, so its window ends it.
#[must_use]
pub fn answered(args: &[String], transcript: &str) -> bool {
    match args.iter().find(|a| a.starts_with('-') && *a != "-i") {
        Some(op) if op == "-L" => !reach_hosts(transcript).is_empty(),
        Some(op) if op == "-G" => !addresses(transcript).is_empty(),
        _ => true,
    }
}

#[cfg(test)]
#[path = "dns_sd_text_tests.rs"]
mod tests;
