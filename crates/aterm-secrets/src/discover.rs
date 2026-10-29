// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `_alab-vault._tcp` mDNS browse and candidate URL ordering for the ALab vault, and the standing
//! browse ([`Watch`]) of that service or of another: the node job's `_alab._tcp`, whose adverts
//! are a fleet's dial hints ([`Watch::adverts`]).
//!
//! Discovered TXT records (`seq`, `signer`, `head`) are ordering hints only: callers verify every
//! fetched envelope against the pinned signer and choose the highest **verified** `seq`. An
//! `_alab._tcp` advert is a hint the same way: whoever dials it still proves who answers.

use crate::server::VaultTxt;
use crate::transport::{self, Source};
use crate::wake::{Bell, Wake};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

/// Cap on candidate URLs probed in one pass.
pub const MAX_CANDIDATES: usize = 16;

/// Cap on the `_alab._tcp` members one joiner's browse resolves and asks: room for every Mac of
/// a lab of about 35, twice over (a member that restarted can still be heard under its old
/// instance), where [`MAX_CANDIDATES`] would leave about 19 of them never asked.
pub const MAX_MEMBERS: usize = 64;

#[cfg(all(target_os = "macos", not(test)))]
const DNS_SD: &str = aterm_uds::os_path!("dns-sd");

#[cfg(all(target_os = "macos", not(test)))]
const POLICY_DENIED: &str = "-65570";

/// One `_alab-vault._tcp` service target resolved on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredVault {
    /// Candidate vault URL (`http://<host>:<port>/v1/<vault>`).
    pub url: String,
    /// Optional parsed TXT hint (`vault`, `signer`, `seq`, `head`), never a trust grant.
    pub txt: Option<VaultTxt>,
}

/// Parse active instance names from a `dns-sd -B _alab-vault._tcp local.` transcript in
/// first-seen order, removing instances whose last interface emitted `Rmv`.
#[must_use]
pub fn parse_browse(text: &str) -> Vec<String> {
    parse_browse_service(text, crate::server::SERVICE_TYPE)
}

/// Parse active instance names from a `dns-sd -B _alab._tcp local.` transcript in first-seen
/// order, removing instances whose last interface emitted `Rmv`, at most [`MAX_MEMBERS`]: whom
/// [`browse_members`] resolves.
#[must_use]
pub fn parse_browse_members(text: &str) -> Vec<String> {
    let mut instances = parse_browse_service(text, MEMBER_SERVICE);
    instances.truncate(MAX_MEMBERS);
    instances
}

fn parse_browse_service(text: &str, expected_service: &str) -> Vec<String> {
    let mut instances: Vec<(String, Vec<u32>)> = Vec::new();
    for line in text.lines() {
        if let Some((add, interface, name)) = browse_row(line, expected_service) {
            apply_row(&mut instances, add, interface, name);
        }
    }
    instances
        .into_iter()
        .filter_map(|(name, interfaces)| (!interfaces.is_empty()).then_some(name))
        .collect()
}

/// One `Add` or `Rmv` row of `service` in a `dns-sd -B <service> local.` transcript: whether it
/// adds, the interface it names and the instance's name. The line may end in the newline a
/// reader keeps (`read_line`), which is no part of the name.
fn browse_row<'a>(line: &'a str, service: &str) -> Option<(bool, u32, &'a str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (cols, name) = columns(line, 6)?;
    let add = match cols[1] {
        "Add" => true,
        "Rmv" => false,
        _ => return None,
    };
    let interface = cols[3].parse::<u32>().ok()?;
    let named = cols[5].strip_suffix('.')?;
    (named == service && !name.is_empty()).then_some((add, interface, name))
}

/// Apply one browse row to `instances` (each name with the interfaces it is heard on, in
/// first-seen order): whether the row added a name, or an interface to one.
fn apply_row(
    instances: &mut Vec<(String, Vec<u32>)>,
    add: bool,
    interface: u32,
    name: &str,
) -> bool {
    let at = instances.iter().position(|(existing, _)| existing == name);
    match (at, add) {
        (Some(i), true) => {
            if instances[i].1.contains(&interface) {
                return false;
            }
            instances[i].1.push(interface);
            true
        }
        (None, true) => {
            instances.push((name.to_owned(), vec![interface]));
            true
        }
        (Some(i), false) => {
            instances[i].1.retain(|&x| x != interface);
            false
        }
        (None, false) => false,
    }
}

/// Parse resolved `(host, port)` targets and optional [`VaultTxt`] hints for `vault` from a
/// `dns-sd -L` transcript.
#[must_use]
pub fn parse_resolve(text: &str, vault: &str) -> Vec<DiscoveredVault> {
    let mut out: Vec<DiscoveredVault> = Vec::new();
    let mut pending: Option<(String, u16)> = None;

    let flush = |out: &mut Vec<DiscoveredVault>, target: (String, u16), txt: Option<VaultTxt>| {
        let (host, port) = target;
        let url = format!("http://{host}:{port}/v1/{vault}");
        if let Some(existing) = out
            .iter_mut()
            .find(|item| item.url.eq_ignore_ascii_case(&url))
        {
            let newer = match (&existing.txt, &txt) {
                (None, Some(_)) => true,
                (Some(old), Some(new)) => new.seq > old.seq,
                _ => false,
            };
            if newer {
                existing.txt = txt;
            }
        } else {
            out.push(DiscoveredVault { url, txt });
        }
    };

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some((_, rest)) = reach_line(line) {
            if let Some(prev) = pending.take() {
                flush(&mut out, prev, None);
            }
            pending = reach_target(rest);
            continue;
        }
        if let Some(target) = pending.take() {
            match classify_txt_line(line, vault) {
                TxtVerdict::NotTxt => {
                    pending = Some(target);
                }
                TxtVerdict::Foreign => {}
                TxtVerdict::Admitted(txt) => flush(&mut out, target, txt),
            }
        }
    }
    if let Some(prev) = pending {
        flush(&mut out, prev, None);
    }
    out
}

/// The service every ALab member's fleet port advertises (`aterm_agent::node_service`), which
/// a joining Mac browses to find the members it asks (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §3.3.1).
pub const MEMBER_SERVICE: &str = "_alab._tcp";

/// The `host:port` targets a `dns-sd -L` transcript resolves, first-seen order, each once.
#[must_use]
pub fn parse_reached(text: &str) -> Vec<String> {
    const REACHED: &str = " can be reached at ";
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find(REACHED) else {
            continue;
        };
        let rest = &line[at + REACHED.len()..];
        let hostport = rest.split(" (interface ").next().unwrap_or(rest).trim();
        let Some((host, port)) = hostport.rsplit_once(':') else {
            continue;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        if port == 0 || !plain_host(&host) {
            continue;
        }
        let target = format!("{host}:{port}");
        if !out.contains(&target) {
            out.push(target);
        }
    }
    out
}

/// What [`browse_members`] says off macOS, which this build browses for members on alone: a
/// member is named by its address instead.
pub const NO_BROWSE: &str = "this Mac cannot browse the network for ALab's members (that needs \
     macOS's Bonjour): name one with `aterm members join --at <host>`";

/// In unit tests: no `dns-sd` runs, and no member is found.
#[cfg(test)]
pub fn browse_members(_budget: Duration) -> crate::Result<Vec<String>> {
    Ok(Vec::new())
}

/// Off macOS no browse runs, so a joining Mac names a member with `--at` ([`NO_BROWSE`]); an
/// empty answer would read as no member on the network.
///
/// # Errors
/// Always: [`NO_BROWSE`].
#[cfg(all(not(test), not(target_os = "macos")))]
pub fn browse_members(_budget: Duration) -> crate::Result<Vec<String>> {
    Err(crate::Error::new(NO_BROWSE))
}

/// Browse [`MEMBER_SERVICE`] on macOS within `budget` and resolve each member's fleet port
/// (`host:port`, at most [`MAX_MEMBERS`]).
///
/// # Errors
/// macOS refused this program the local network.
#[cfg(all(target_os = "macos", not(test)))]
pub fn browse_members(budget: Duration) -> crate::Result<Vec<String>> {
    let started = Instant::now();
    let (out, err) = run_dns_sd(&["-B", MEMBER_SERVICE, "local."], budget / 2, |_| false)?;
    let said = format!("{out}\n{err}");
    if said
        .lines()
        .filter(|line| !is_browse_row(line))
        .any(|line| line.split_whitespace().any(|word| word == POLICY_DENIED))
    {
        return Err(crate::Error::new(format!(
            "browsing {MEMBER_SERVICE}: {}",
            aterm_http::LOCAL_NETWORK_REFUSED
        )));
    }
    let instances = parse_browse_members(&out);
    let left = budget.saturating_sub(started.elapsed());
    let resolved: Vec<Vec<String>> = std::thread::scope(|scope| {
        let resolving: Vec<_> = instances
            .iter()
            .map(|name| {
                scope.spawn(move || {
                    run_dns_sd(
                        &["-L", name.as_str(), MEMBER_SERVICE, "local."],
                        left,
                        |line| line.contains(" can be reached at "),
                    )
                    .map(|(text, _)| parse_reached(&text))
                    .unwrap_or_default()
                })
            })
            .collect();
        resolving
            .into_iter()
            .map(|resolve| resolve.join().unwrap_or_default())
            .collect()
    });
    let mut found: Vec<String> = Vec::new();
    for target in resolved.into_iter().flatten() {
        if !found.contains(&target) {
            found.push(target);
        }
    }
    Ok(found)
}

/// Whether a `_alab-vault._tcp` instance name is `alab-secrets` or a Bonjour collision rename of
/// it (`alab-secrets (2)`, `alab-secrets (3)`, …; `DESIGN-fluid-swarm` §4.1 F7).
#[must_use]
pub fn is_alab_holder_instance(name: &str) -> bool {
    let trimmed = name.trim();
    let alab = crate::alab_advertised();
    if trimmed.eq_ignore_ascii_case(alab) {
        return true;
    }
    if trimmed.len() <= alab.len() + 3 {
        return false;
    }
    // A name from the LAN can put a multibyte character across the cut: that is not a holder.
    let Some((prefix, rest)) = trimmed.split_at_checked(alab.len()) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(alab) {
        return false;
    }
    let Some(digits) = rest.strip_prefix(" (").and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Parse resolved `alab-secrets` holder targets and [`VaultTxt`] hints from `dns-sd -L` output
/// without collapsing equal-URL collisions (`DESIGN-fluid-swarm` §4.1 F7: two holders after a heal
/// both resolve `alab-secrets.local:7484`, distinguished by `label=` in their TXT).
#[must_use]
pub fn parse_alab_holders(text: &str) -> Vec<DiscoveredVault> {
    let vault = crate::ALAB_VAULT;
    let mut out: Vec<DiscoveredVault> = Vec::new();
    let mut pending: Option<(String, u16)> = None;

    let flush = |out: &mut Vec<DiscoveredVault>, target: (String, u16), txt: Option<VaultTxt>| {
        let (host, port) = target;
        let url = format!("http://{host}:{port}/v1/{vault}");
        let item = DiscoveredVault { url, txt };
        if !out.contains(&item) {
            out.push(item);
        }
    };

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some((_, rest)) = reach_line(line) {
            if let Some(prev) = pending.take() {
                flush(&mut out, prev, None);
            }
            pending = reach_target(rest);
            continue;
        }
        if let Some(target) = pending.take() {
            match classify_txt_line(line, vault) {
                TxtVerdict::NotTxt => {
                    pending = Some(target);
                }
                TxtVerdict::Foreign => {}
                TxtVerdict::Admitted(txt) => flush(&mut out, target, txt),
            }
        }
    }
    if let Some(prev) = pending {
        flush(&mut out, prev, None);
    }
    out
}

/// A `dns-sd -L` line that says an instance can be reached: what comes before
/// ` can be reached at ` (the timestamp and the instance's full name) and what comes after.
fn reach_line(line: &str) -> Option<(&str, &str)> {
    const REACHED: &str = " can be reached at ";
    let at = line.find(REACHED)?;
    Some((&line[..at], &line[at + REACHED.len()..]))
}

/// The host, lowercase without its trailing dot, and the port a reach line names after
/// ` can be reached at ` (`m3.local.:7484 (interface 9) Flags: 1`). `None` for port 0, or a host
/// of other characters than a DNS name's.
fn reach_target(rest: &str) -> Option<(String, u16)> {
    let hostport = rest.split(" (interface ").next().unwrap_or(rest).trim();
    let (host, port) = hostport.rsplit_once(':')?;
    let host = host.trim_end_matches('.');
    let port = port.parse::<u16>().ok()?;
    (port != 0 && plain_host(host)).then(|| (host.to_ascii_lowercase(), port))
}

/// One instance's advert as `dns-sd -L` resolves it: where it can be reached and the TXT it
/// carries. The node job's `_alab._tcp` advert is a fleet member's dial hint: its port is the
/// fleet port and its TXT is `v=1 role=<member|steward> fleet=<port> window=<0|1>`. A hint only,
/// as a vault's TXT is: whoever dials it still proves who answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advert {
    /// The instance's name, as the browse heard it (`alab-1a2b3c4d`).
    pub instance: String,
    /// The host it can be reached at, lowercase, without the trailing dot (`m3.local`).
    pub host: String,
    /// The port it registered.
    pub port: u16,
    /// Its TXT strings as `key=value` pairs, in order. A string with no `=` is left out.
    pub txt: Vec<(String, String)>,
}

/// Parse `instance`'s advert from a `dns-sd -L <instance> <service> local.` transcript: the first
/// line that says `instance` can be reached, its full name as `dns-sd` prints it (a space as
/// `\032`, a dot as `\.`), and the TXT line under it, if any. `None` with no such line, or with
/// only lines whose host or port is unusable.
#[must_use]
pub fn parse_advert(instance: &str, transcript: &str) -> Option<Advert> {
    let mut lines = transcript.lines().map(|line| line.trim_end_matches('\r'));
    while let Some(line) = lines.next() {
        let Some((head, rest)) = reach_line(line) else {
            continue;
        };
        if !names_instance(full_name(head), instance) {
            continue;
        }
        let Some((host, port)) = reach_target(rest) else {
            continue;
        };
        // `dns-sd` prints the TXT on the next line, each string after a space.
        let txt = lines
            .next()
            .filter(|next| next.starts_with([' ', '\t']))
            .map(txt_pairs)
            .unwrap_or_default();
        return Some(Advert {
            instance: instance.to_owned(),
            host,
            port,
            txt,
        });
    }
    None
}

/// The full name in what a reach line says before ` can be reached at `, past its timestamp
/// (` 6:21:48.497  m3._alab._tcp.local.`).
fn full_name(head: &str) -> &str {
    let head = head.trim();
    match head.split_once(char::is_whitespace) {
        Some((stamp, name))
            if !stamp.is_empty()
                && stamp
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b':' | b'.')) =>
        {
            name.trim()
        }
        _ => head,
    }
}

/// Whether `full` (`<instance>.<service>.<domain>.`, as `dns-sd -L` prints it: a byte as `\DDD`
/// in decimal, a dot or backslash in the name after a `\`) names `instance`, ignoring ASCII case
/// as DNS does.
fn names_instance(full: &str, instance: &str) -> bool {
    let raw = full.as_bytes();
    let mut label = Vec::with_capacity(raw.len());
    let mut at = 0;
    while let Some(&byte) = raw.get(at) {
        match byte {
            b'.' => break,
            b'\\' => {
                let digits = raw
                    .get(at + 1..at + 4)
                    .filter(|digits| digits.iter().all(u8::is_ascii_digit));
                if let Some(digits) = digits {
                    let value = digits
                        .iter()
                        .fold(0_u32, |n, digit| n * 10 + u32::from(digit - b'0'));
                    let Ok(value) = u8::try_from(value) else {
                        return false;
                    };
                    label.push(value);
                    at += 4;
                } else if let Some(&next) = raw.get(at + 1) {
                    label.push(next);
                    at += 2;
                } else {
                    return false;
                }
            }
            _ => {
                label.push(byte);
                at += 1;
            }
        }
    }
    label.eq_ignore_ascii_case(instance.as_bytes())
}

/// The `key=value` pairs of a TXT line as `dns-sd` prints it: each string after a space, a space
/// or shell metacharacter in a string after a `\`.
fn txt_pairs(line: &str) -> Vec<(String, String)> {
    let mut strings: Vec<String> = Vec::new();
    let mut string = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => string.extend(chars.next()),
            c if c.is_whitespace() => {
                if !string.is_empty() {
                    strings.push(std::mem::take(&mut string));
                }
            }
            c => string.push(c),
        }
    }
    if !string.is_empty() {
        strings.push(string);
    }
    strings
        .iter()
        .filter_map(|string| {
            let (key, value) = string.split_once('=')?;
            (!key.is_empty()).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

enum TxtVerdict {
    NotTxt,
    Foreign,
    Admitted(Option<VaultTxt>),
}

fn classify_txt_line(line: &str, expected_vault: &str) -> TxtVerdict {
    let mut saw_pair = false;
    let mut path = None;
    let mut named_vault = None;
    for word in line.split_whitespace() {
        let Some((key, value)) = word.split_once('=') else {
            continue;
        };
        saw_pair = true;
        if key.eq_ignore_ascii_case("path") {
            path.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("vault") {
            named_vault.get_or_insert(value);
        }
    }
    if !saw_pair {
        return TxtVerdict::NotTxt;
    }
    if path.is_some_and(|p| p != "/v1") {
        return TxtVerdict::Foreign;
    }
    if named_vault.is_some_and(|v| v != expected_vault) {
        return TxtVerdict::Foreign;
    }
    let parsed = VaultTxt::from_resolve_line(line);
    if parsed.as_ref().is_some_and(|t| t.vault != expected_vault) {
        return TxtVerdict::Foreign;
    }
    TxtVerdict::Admitted(parsed)
}

/// Build the deduplicated candidate URL list for `vault`: `defaults` first in compiled-in order,
/// followed by `discovered` ordered by advertised TXT `seq` descending, capped at
/// [`MAX_CANDIDATES`].
#[must_use]
pub fn candidate_urls(
    defaults: &[&str],
    discovered: &[DiscoveredVault],
    vault: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push_valid = |raw: &str| {
        if out.len() >= MAX_CANDIDATES {
            return;
        }
        let Ok((Source::Url(clean), None)) = transport::parse_source(raw, Path::new("/")) else {
            return;
        };
        if transport::vault_name(&Source::Url(clean.clone()))
            .ok()
            .as_deref()
            != Some(vault)
        {
            return;
        }
        if !out.iter().any(|existing| same_endpoint(existing, &clean)) {
            out.push(clean);
        }
    };
    for &url in defaults {
        push_valid(url);
    }
    let mut ranked: Vec<&DiscoveredVault> = discovered.iter().collect();
    ranked.sort_by(|a, b| {
        let seq_a = a.txt.as_ref().map_or(0, |t| t.seq);
        let seq_b = b.txt.as_ref().map_or(0, |t| t.seq);
        seq_b.cmp(&seq_a)
    });
    for item in ranked {
        push_valid(&item.url);
    }
    out
}

fn same_endpoint(a: &str, b: &str) -> bool {
    transport::describe(a).eq_ignore_ascii_case(&transport::describe(b))
}

fn plain_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn columns(line: &str, n: usize) -> Option<(Vec<&str>, &str)> {
    let mut cols = Vec::with_capacity(n);
    let mut rest = line;
    for _ in 0..n {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        cols.push(&rest[..end]);
        rest = &rest[end..];
    }
    Some((cols, rest.trim_start()))
}

#[cfg(all(target_os = "macos", not(test)))]
fn is_browse_row(line: &str) -> bool {
    columns(line, 6).is_some_and(|(cols, _)| matches!(cols[1], "Add" | "Rmv"))
}

/// Browse `_alab-vault._tcp` on macOS within `budget` and resolve advertised targets for `vault`.
///
/// In unit tests and off macOS this touches neither `dns-sd` nor the network and returns an empty
/// list; tests exercise discovery and candidate selection deterministically through
/// [`parse_browse`], [`parse_resolve`], [`candidate_urls`] and `client::probe_candidates_with`.
#[cfg(any(test, not(target_os = "macos")))]
pub fn browse_alab(_budget: std::time::Duration) -> crate::Result<Vec<DiscoveredVault>> {
    Ok(Vec::new())
}

/// Browse `_alab-vault._tcp` on macOS within `budget` and resolve advertised targets for `alab`.
#[cfg(all(target_os = "macos", not(test)))]
pub fn browse_alab(budget: Duration) -> crate::Result<Vec<DiscoveredVault>> {
    if budget.is_zero() {
        return Ok(Vec::new());
    }
    let started = Instant::now();
    let browse_budget = (budget / 4).clamp(Duration::from_millis(50), Duration::from_millis(250));
    let (out, err) = run_dns_sd(
        &["-B", crate::server::SERVICE_TYPE, "local."],
        browse_budget,
        |_| false,
    )?;
    let said = format!("{out}\n{err}");
    let denied = said
        .lines()
        .filter(|line| !is_browse_row(line))
        .any(|line| line.split_whitespace().any(|word| word == POLICY_DENIED));
    if denied {
        return Err(crate::Error::new(format!(
            "browsing {}: {}",
            crate::server::SERVICE_TYPE,
            aterm_http::LOCAL_NETWORK_REFUSED
        )));
    }
    let mut instances = parse_browse(&out);
    instances.truncate(MAX_CANDIDATES);
    if instances.is_empty() {
        return Ok(Vec::new());
    }
    let left = budget.saturating_sub(started.elapsed());
    if left < Duration::from_millis(40) {
        return Ok(Vec::new());
    }
    Ok(resolve_all(&instances, left))
}

/// Resolve each of `instances` (`dns-sd -L`) within `budget`, all at once, into the targets
/// they advertise for `alab`, one per endpoint (the highest TXT `seq` kept).
#[cfg(all(target_os = "macos", not(test)))]
fn resolve_all(instances: &[String], budget: Duration) -> Vec<DiscoveredVault> {
    let resolve_budget = (budget / 3).clamp(Duration::from_millis(40), Duration::from_millis(250));
    let resolved: Vec<Vec<DiscoveredVault>> = std::thread::scope(|scope| {
        let handles: Vec<_> = instances
            .iter()
            .map(|name| {
                scope.spawn(move || {
                    let mut saw_reach = false;
                    run_dns_sd(
                        &["-L", name.as_str(), crate::server::SERVICE_TYPE, "local."],
                        resolve_budget,
                        |line| {
                            if line.contains(" can be reached at ") {
                                saw_reach = true;
                                false
                            } else {
                                saw_reach && line.contains('=')
                            }
                        },
                    )
                    .map(|(text, _)| parse_resolve(&text, crate::ALAB_VAULT))
                    .unwrap_or_default()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect()
    });
    let mut merged: Vec<DiscoveredVault> = Vec::new();
    for item in resolved.into_iter().flatten() {
        if let Some(existing) = merged
            .iter_mut()
            .find(|prev| same_endpoint(&prev.url, &item.url))
        {
            let newer = match (&existing.txt, &item.txt) {
                (None, Some(_)) => true,
                (Some(old), Some(new)) => new.seq > old.seq,
                _ => false,
            };
            if newer {
                existing.txt = item.txt;
            }
        } else {
            merged.push(item);
        }
    }
    merged
}

/// A test build, or one off macOS, resolves nothing: no `dns-sd` runs.
#[cfg(any(test, not(target_os = "macos")))]
fn resolve_all(_instances: &[String], _budget: Duration) -> Vec<DiscoveredVault> {
    Vec::new()
}

#[cfg(all(target_os = "macos", not(test)))]
fn resolve_alab_holders(instances: &[String], budget: Duration) -> Vec<DiscoveredVault> {
    let resolve_budget = (budget / 3).clamp(Duration::from_millis(40), Duration::from_millis(250));
    let resolved: Vec<Vec<DiscoveredVault>> = std::thread::scope(|scope| {
        let handles: Vec<_> = instances
            .iter()
            .map(|name| {
                scope.spawn(move || {
                    let mut saw_reach = false;
                    run_dns_sd(
                        &["-L", name.as_str(), crate::server::SERVICE_TYPE, "local."],
                        resolve_budget,
                        |line| {
                            if line.contains(" can be reached at ") {
                                saw_reach = true;
                                false
                            } else {
                                saw_reach && line.contains('=')
                            }
                        },
                    )
                    .map(|(text, _)| parse_alab_holders(&text))
                    .unwrap_or_default()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect()
    });
    let mut out = Vec::new();
    for item in resolved.into_iter().flatten() {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

#[cfg(any(test, not(target_os = "macos")))]
fn resolve_alab_holders(_instances: &[String], _budget: Duration) -> Vec<DiscoveredVault> {
    Vec::new()
}

/// Resolve each of `instances` of `service` (`dns-sd -L`), at most [`MAX_CANDIDATES`] at once, all
/// within `budget`, into the adverts they answer with, in the order of `instances`. One that does
/// not answer before its share of the budget ends is left out, and none starts once less than
/// 40 ms of it is left.
#[cfg(all(target_os = "macos", not(test)))]
fn resolve_adverts(service: &str, instances: &[String], budget: Duration) -> Vec<Advert> {
    use std::sync::atomic::AtomicUsize;
    let deadline = Instant::now().checked_add(budget);
    let each = (budget / 3).clamp(Duration::from_millis(40), Duration::from_millis(250));
    let next = AtomicUsize::new(0);
    let resolved: Vec<Vec<(usize, Advert)>> = std::thread::scope(|scope| {
        let next = &next;
        let workers: Vec<_> = (0..instances.len().min(MAX_CANDIDATES))
            .map(|_| {
                scope.spawn(move || {
                    let mut got = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(name) = instances.get(at) else {
                            return got;
                        };
                        let left = deadline.map_or(budget, |deadline| {
                            deadline.saturating_duration_since(Instant::now())
                        });
                        if left < Duration::from_millis(40) {
                            return got;
                        }
                        let mut saw_reach = false;
                        let ran = run_dns_sd(
                            &["-L", name.as_str(), service, "local."],
                            each.min(left),
                            |line| {
                                if line.contains(" can be reached at ") {
                                    saw_reach = true;
                                    false
                                } else {
                                    saw_reach && line.contains('=')
                                }
                            },
                        );
                        if let Some(advert) =
                            ran.ok().and_then(|(text, _)| parse_advert(name, &text))
                        {
                            got.push((at, advert));
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap_or_default())
            .collect()
    });
    let mut resolved: Vec<(usize, Advert)> = resolved.into_iter().flatten().collect();
    resolved.sort_by_key(|(at, _)| *at);
    resolved.into_iter().map(|(_, advert)| advert).collect()
}

/// A test build, or one off macOS, resolves nothing: no `dns-sd` runs.
#[cfg(any(test, not(target_os = "macos")))]
fn resolve_adverts(_service: &str, _instances: &[String], _budget: Duration) -> Vec<Advert> {
    Vec::new()
}

/// How long a browse child's first rows are its baseline on a watch of a service other than the
/// vault's (the node job's `_alab._tcp`): what it hears then was there before this process
/// looked, so it is held but rings nothing.
const SETTLE: Duration = Duration::from_secs(30);

/// The most instances a watch of a service other than the vault's holds: one more is neither
/// held nor rung for (mDNS is anyone's to write on the LAN).
const MOST_HELD: usize = 256;

/// How soon after an instance left an interface its return there is it re-registering, no
/// return, on a watch of a service other than the vault's: a peer's window opening or closing
/// restarts its node's registration under the same name, a removal and an add moments apart.
pub const REREGISTERING_WITHIN: Duration = Duration::from_secs(2 * 60);

/// A watch of `service`'s settle window and the most instances it holds. The vault's watch rings
/// from a browse child's first row and holds every instance it hears, as it always has; a watch
/// of another service takes [`SETTLE`] and [`MOST_HELD`].
fn holding(service: &str) -> (Duration, usize) {
    if service == crate::server::SERVICE_TYPE {
        (Duration::ZERO, usize::MAX)
    } else {
        (SETTLE, MOST_HELD)
    }
}

/// A standing `dns-sd -B <service> local.` (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.2):
/// the vault's `_alab-vault._tcp`, beside the bounded [`browse_alab`] ([`Watch::start`]), or the
/// node job's `_alab._tcp`, whose adverts are the fleet's dial hints ([`Watch::start_for`],
/// [`Watch::adverts`]). It keeps the browse running, starting it again when it ends, and keeps
/// the instances it hears, each forgotten at the `Rmv` of the last interface it was heard on. At
/// each Add of an instance, or (the vault's) of one on an interface it was not heard on, it rings
/// its bell with [`Wake::Browse`]: a server or member that came back, or a new one; another
/// service's watch passes over a member back within [`REREGISTERING_WITHIN`], which is
/// re-registering. Adds of this
/// process's own instances ring nothing, and nor do those a browse child hears within the
/// watch's settle window. Dropping it ends the browse.
pub struct Watch {
    watched: Arc<Watched>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("service", &self.watched.service)
            .field("heard", &self.heard())
            .finish_non_exhaustive()
    }
}

/// A test's resolver for [`Watch::adverts`] ([`Watch::idle_resolving`]).
#[cfg(any(test, debug_assertions))]
type Resolver = Box<dyn Fn(&str) -> Option<Advert> + Send + Sync>;

struct Watched {
    /// The service browsed (`_alab-vault._tcp`, `_alab._tcp`).
    service: &'static str,
    /// How long a browse child's first rows are its baseline ([`holding`]).
    settle: Duration,
    /// The most instances held ([`holding`]).
    most: usize,
    /// Each instance heard, with the interfaces it is heard on.
    instances: Mutex<Vec<(String, Vec<u32>)>>,
    /// When the running browse child began, and how long before that it is taken to have
    /// begun (a test's seam, `Watch::child_began`); `None` while none has (an idle watch).
    began: Mutex<Option<(Instant, Duration)>>,
    bell: Weak<Bell>,
    /// This process's own instances.
    own: Mutex<Vec<String>>,
    /// When each instance left an interface within the last [`REREGISTERING_WITHIN`], at most
    /// [`Watched::most`] of them (none kept on the vault's watch).
    gone: Mutex<Vec<((String, u32), Instant)>>,
    /// Where the next [`Watch::found`] starts its cut of what is heard ([`rotated`]).
    found_from: AtomicUsize,
    /// Where the next [`Watch::peer_vaults`] starts its cut of what is heard ([`rotated`]).
    peers_from: AtomicUsize,
    closed: AtomicBool,
    /// The `dns-sd -B` running now, killed when the watch is dropped.
    child: Mutex<Option<aterm_uds::spawnfd::Child>>,
    /// What [`Watch::adverts`] resolves through in place of `dns-sd -L`, a test's.
    #[cfg(any(test, debug_assertions))]
    resolve: Option<Resolver>,
}

impl Watch {
    /// Start a standing `_alab-vault._tcp` browse that rings `bell` ([`Watch::start_for`]).
    #[must_use]
    pub fn start(bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch::start_for(crate::server::SERVICE_TYPE, bell, own)
    }

    /// Start a standing browse of `service` that rings `bell`. `own` names this process's own
    /// instances. A watch of the vault's `_alab-vault._tcp` rings from each browse child's first
    /// row and holds every instance it hears; a watch of another service (the node job's
    /// `_alab._tcp`) holds what a child hears in its first 30 s as its baseline, ringing
    /// nothing for it, and holds at most 256 instances. In a test build, and off macOS, no
    /// browse runs: the watch is [`Watch::idle_for`].
    #[must_use]
    pub fn start_for(service: &'static str, bell: &Arc<Bell>, own: &[&str]) -> Watch {
        let watch = Watch::idle_for(service, bell, own);
        #[cfg(all(target_os = "macos", not(test)))]
        {
            let watched = Arc::clone(&watch.watched);
            if let Err(error) = std::thread::Builder::new()
                .name(format!("browse {service}"))
                .spawn(move || browse_standing(&watched))
            {
                crate::log_line(&format!("could not start browsing {service}: {error}"));
            }
        }
        watch
    }

    /// An `_alab-vault._tcp` watch that runs no browse ([`Watch::idle_for`]).
    #[must_use]
    pub fn idle(bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch::idle_for(crate::server::SERVICE_TYPE, bell, own)
    }

    /// A watch of `service` that runs no browse: rows reach it only through [`Watch::feed`], and
    /// with no browse child begun, none of them is a baseline.
    #[must_use]
    pub fn idle_for(service: &'static str, bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch {
            watched: Arc::new(Watched::new(service, bell, own)),
        }
    }

    /// Seam: [`Watch::idle_for`] whose [`Watch::adverts`] resolves each instance with `resolve`
    /// in place of `dns-sd -L`, for a test that feeds the watch rows and reads its dial hints.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn idle_resolving(
        service: &'static str,
        bell: &Arc<Bell>,
        own: &[&str],
        resolve: impl Fn(&str) -> Option<Advert> + Send + Sync + 'static,
    ) -> Watch {
        let mut watched = Watched::new(service, bell, own);
        watched.resolve = Some(Box::new(resolve));
        Watch {
            watched: Arc::new(watched),
        }
    }

    /// Seam: take it that a browse child began `ago` ago, as the standing browse does at each
    /// child it starts with `ago` zero: rows fed while the child is younger than the watch's
    /// settle window are its baseline, held but ringing nothing.
    #[cfg(any(test, debug_assertions))]
    pub fn child_began(&self, ago: Duration) {
        self.watched.begin(ago);
    }

    /// Record an instance name advertised by this process so its own `Add` rows do not wake it.
    pub fn note_own(&self, name: &str) {
        if name.is_empty() {
            return;
        }
        let mut own = self
            .watched
            .own
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !own
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            own.push(name.to_owned());
        }
    }

    /// Take one line of the browse's transcript: an `Add` or `Rmv` row of the watch's service
    /// (as [`parse_browse`] reads the vault's), with or without its newline.
    pub fn feed(&self, line: &str) {
        if let Some((add, interface, name)) = browse_row(line, self.watched.service) {
            self.watched.hear(add, interface, name, Instant::now());
        }
    }

    /// Take one event of a browse of the watch's service that its process runs itself (a native
    /// Bonjour connection, which [`Watch::idle_for`] leaves to it): instance `name` added
    /// (`added`) or removed on `interface`, as [`Watch::feed`] takes a row.
    pub fn hear(&self, added: bool, interface: u32, name: &str) {
        self.watched.hear(added, interface, name, Instant::now());
    }

    /// Seam: [`Watch::hear`] at `now`, which times a re-registration ([`REREGISTERING_WITHIN`]);
    /// the settle window still runs on the clock ([`Watch::child_began`]).
    #[cfg(any(test, debug_assertions))]
    pub fn hear_at(&self, added: bool, interface: u32, name: &str, now: Instant) {
        self.watched.hear(added, interface, name, now);
    }

    /// A browse its process runs itself ([`Watch::hear`]) began, or began again after it ended:
    /// what the last one heard is forgotten, and what this one hears within the settle window is
    /// its baseline, as at each browse child [`Watch::start_for`] starts.
    pub fn began_browse(&self) {
        self.watched.lock().clear();
        self.watched
            .gone
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.watched.begin(Duration::ZERO);
    }

    /// The instances heard now, in first-seen order.
    #[must_use]
    pub fn heard(&self) -> Vec<String> {
        self.watched
            .lock()
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Resolve the instances heard now, at most [`MAX_CANDIDATES`] ([`rotated`]: each call takes
    /// the next of them), within `budget`: the targets they advertise for `alab`. A test build
    /// resolves nothing, and nor does a watch of another service than `_alab-vault._tcp`, whose
    /// instances are no vault servers.
    #[must_use]
    pub fn found(&self, budget: Duration) -> Vec<DiscoveredVault> {
        if !self.watched.is_vault() {
            return Vec::new();
        }
        let names = self.found_names();
        if names.is_empty() {
            return Vec::new();
        }
        resolve_all(&names, budget)
    }

    /// The instances [`Watch::found`] resolves now.
    fn found_names(&self) -> Vec<String> {
        rotated(self.heard(), &self.watched.found_from)
    }

    /// Resolve peer instances heard now (excluding this server's own primary instance names), at
    /// most [`MAX_CANDIDATES`] ([`rotated`]: each call takes the next of them), within `budget`
    /// (`DESIGN-fluid-swarm` §4.1 F6). A watch of another service than `_alab-vault._tcp`
    /// resolves none.
    #[must_use]
    pub fn peer_vaults(&self, budget: Duration) -> Vec<DiscoveredVault> {
        if !self.watched.is_vault() {
            return Vec::new();
        }
        let names = self.peer_names();
        if names.is_empty() {
            return Vec::new();
        }
        resolve_all(&names, budget)
    }

    /// The instances [`Watch::peer_vaults`] resolves now.
    fn peer_names(&self) -> Vec<String> {
        let own_snapshot = self
            .watched
            .own
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let names: Vec<String> = self
            .heard()
            .into_iter()
            .filter(|name| {
                !own_snapshot
                    .iter()
                    .any(|own| own != crate::alab_advertised() && own.eq_ignore_ascii_case(name))
            })
            .collect();
        rotated(names, &self.watched.peers_from)
    }

    /// Resolve all `alab-secrets` / `alab-secrets (<N>)` instances currently heard on the LAN
    /// without collapsing equal-URL collisions (`DESIGN-fluid-swarm` §4.1 F7). A watch of another
    /// service than `_alab-vault._tcp` resolves none.
    #[must_use]
    pub fn alab_holders(&self, budget: Duration) -> Vec<DiscoveredVault> {
        if !self.watched.is_vault() {
            return Vec::new();
        }
        let mut names: Vec<String> = self
            .heard()
            .into_iter()
            .filter(|name| is_alab_holder_instance(name))
            .collect();
        names.truncate(MAX_CANDIDATES);
        if names.is_empty() {
            return Vec::new();
        }
        resolve_alab_holders(&names, budget)
    }

    /// Resolve each instance held now but this process's own (`dns-sd -L <instance> <service>
    /// local.`), at most [`MAX_CANDIDATES`] at once, all within `budget`: where each can be
    /// reached and the TXT it carries ([`parse_advert`]), in first-heard order. One that does not
    /// answer in time is left out. On the node job's `_alab._tcp` watch these are the fleet's
    /// dial hints. A test build, and one off macOS, resolves nothing, except through the
    /// resolver of a watch a test made with `Watch::idle_resolving`.
    #[must_use]
    pub fn adverts(&self, budget: Duration) -> Vec<Advert> {
        let mut names = self.heard();
        {
            let own = self
                .watched
                .own
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            names.retain(|name| !own.iter().any(|own| own.eq_ignore_ascii_case(name)));
        }
        if names.is_empty() {
            return Vec::new();
        }
        #[cfg(any(test, debug_assertions))]
        if let Some(resolve) = &self.watched.resolve {
            return names.iter().filter_map(|name| resolve(name)).collect();
        }
        resolve_adverts(self.watched.service, &names, budget)
    }
}

/// At most [`MAX_CANDIDATES`] of `names` (in first-heard order), starting at `from` and moving it
/// on by as many, so each is among them within `ceil(len / MAX_CANDIDATES)` calls. About 35 Macs
/// serve: the first 16 heard, every pass, left a Mac heard later (a laptop back after its name
/// expired is heard last everywhere) out of every server's pull and every member's failover, so
/// records only its own server held reached no other.
fn rotated(mut names: Vec<String>, from: &AtomicUsize) -> Vec<String> {
    if names.len() > MAX_CANDIDATES {
        let start = from.fetch_add(MAX_CANDIDATES, Ordering::Relaxed) % names.len();
        names.rotate_left(start);
        names.truncate(MAX_CANDIDATES);
    }
    names
}

impl Watched {
    fn new(service: &'static str, bell: &Arc<Bell>, own: &[&str]) -> Watched {
        let (settle, most) = holding(service);
        Watched {
            service,
            settle,
            most,
            instances: Mutex::new(Vec::new()),
            began: Mutex::new(None),
            bell: Arc::downgrade(bell),
            own: Mutex::new(own.iter().map(|name| (*name).to_owned()).collect()),
            gone: Mutex::new(Vec::new()),
            found_from: AtomicUsize::new(0),
            peers_from: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            child: Mutex::new(None),
            #[cfg(any(test, debug_assertions))]
            resolve: None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(String, Vec<u32>)>> {
        self.instances
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this is the vault's `_alab-vault._tcp` watch.
    fn is_vault(&self) -> bool {
        self.service == crate::server::SERVICE_TYPE
    }

    /// A browse began `ago` ago: what it hears while younger than [`Watched::settle`] is its
    /// baseline.
    fn begin(&self, ago: Duration) {
        *self.began.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), ago));
    }

    /// Whether a row heard now is the running browse child's baseline.
    fn baseline(&self) -> bool {
        self.began
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|(at, ago)| ago.saturating_add(at.elapsed()) < self.settle)
    }

    fn is_own(&self, name: &str) -> bool {
        self.own
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|own| own == name)
    }

    /// Take one row heard at `now`. The vault's watch rings at each add of an instance, or of one
    /// on an interface it was not heard on; another service's watch only at an add of an
    /// instance it does not hold that did not leave that interface within
    /// [`REREGISTERING_WITHIN`] (one heard on another interface is the same member, and one back
    /// that soon is re-registering). Neither rings in its settle window or for its own.
    fn hear(&self, add: bool, interface: u32, name: &str, now: Instant) {
        if name.is_empty() {
            return;
        }
        let (added, new) = {
            let mut instances = self.lock();
            let held = instances.iter().any(|(held, _)| held == name);
            if add && instances.len() >= self.most && !held {
                return;
            }
            let was_on = instances
                .iter()
                .any(|(held, on)| held == name && on.contains(&interface));
            let added = apply_row(&mut instances, add, interface, name);
            instances.retain(|(_, interfaces)| !interfaces.is_empty());
            if !add && was_on && !self.is_vault() {
                self.went(name, interface, now);
            }
            (added, add && !held)
        };
        let rings = if self.is_vault() {
            added
        } else {
            let back = add && self.back(name, interface, now);
            new && !back
        };
        if rings
            && !self.baseline()
            && !self.is_own(name)
            && let Some(bell) = self.bell.upgrade()
        {
            bell.ring(Wake::Browse);
        }
    }

    /// Note `name`'s removal from `interface` at `now`, forgetting removals older than
    /// [`REREGISTERING_WITHIN`] and, past [`Watched::most`], the oldest.
    fn went(&self, name: &str, interface: u32, now: Instant) {
        let mut gone = self.gone.lock().unwrap_or_else(PoisonError::into_inner);
        gone.retain(|((held, on), at)| {
            !(held == name && *on == interface)
                && now.saturating_duration_since(*at) <= REREGISTERING_WITHIN
        });
        if gone.len() >= self.most {
            gone.remove(0);
        }
        gone.push(((name.to_owned(), interface), now));
    }

    /// Whether an add of `name` on `interface` at `now` comes within [`REREGISTERING_WITHIN`] of
    /// its removal there: an instance re-registering, not one back. It forgets that removal
    /// either way.
    fn back(&self, name: &str, interface: u32, now: Instant) -> bool {
        let mut gone = self.gone.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(at) = gone
            .iter()
            .position(|((held, on), _)| held == name && *on == interface)
        else {
            return false;
        };
        let (_, went) = gone.remove(at);
        now.saturating_duration_since(went) <= REREGISTERING_WITHIN
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.watched.closed.store(true, Ordering::SeqCst);
        if let Some(mut child) = self
            .watched
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The first pause before a standing browse that ended starts again, doubled each time it ends
/// within [`RESTART_LONGEST`] of its start, up to that.
#[cfg(all(target_os = "macos", not(test)))]
const RESTART_FIRST: Duration = Duration::from_secs(5);
#[cfg(all(target_os = "macos", not(test)))]
const RESTART_LONGEST: Duration = Duration::from_secs(60);

/// Run `dns-sd -B <service> local.` for `watched` until the watch is dropped, starting it again
/// whenever it ends (macOS's mDNSResponder restarted, or refused this process the local network),
/// and feed it each row. A browse that ended heard nothing more, so what it heard is forgotten,
/// and the next child's settle window begins with it.
#[cfg(all(target_os = "macos", not(test)))]
fn browse_standing(watched: &Watched) {
    use aterm_uds::spawnfd::Stdio;
    use std::io::BufRead as _;
    let mut pause = RESTART_FIRST;
    while !watched.closed.load(Ordering::SeqCst) {
        let started = Instant::now();
        let spawned = aterm_uds::spawnfd::command(DNS_SD)
            .args(["-B", watched.service, "local."])
            .setsid(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            watched.begin(Duration::ZERO);
            let stdout = child.stdout.take();
            *watched.child.lock().unwrap_or_else(PoisonError::into_inner) = Some(child);
            // A drop between the start and the store above found no child to kill.
            if watched.closed.load(Ordering::SeqCst) {
                end_browse(watched);
                return;
            }
            if let Some(stdout) = stdout {
                let mut reader = std::io::BufReader::new(stdout);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if let Some((add, interface, name)) = browse_row(&line, watched.service) {
                        watched.hear(add, interface, name, Instant::now());
                    }
                    line.clear();
                }
            }
            end_browse(watched);
            watched.lock().clear();
        }
        if started.elapsed() >= RESTART_LONGEST {
            pause = RESTART_FIRST;
        }
        let deadline = Instant::now() + pause;
        while !watched.closed.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(1).min(pause));
        }
        pause = pause.saturating_mul(2).min(RESTART_LONGEST);
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn end_browse(watched: &Watched) {
    if let Some(mut child) = watched
        .child
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn run_dns_sd(
    args: &[&str],
    budget: Duration,
    mut enough: impl FnMut(&str) -> bool,
) -> crate::Result<(String, String)> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::BufRead as _;
    use std::sync::mpsc;

    let mut child = aterm_uds::spawnfd::command(DNS_SD)
        .args(args)
        .setsid(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            crate::Error::new(format!(
                "dns-sd {}: {e}",
                args.first().copied().unwrap_or("")
            ))
        })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel::<String>();
    let out_handle = std::thread::spawn(move || {
        let mut collected = String::new();
        if let Some(stdout) = stdout {
            let mut reader = std::io::BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                collected.push_str(&line);
                let _ = tx.send(line.clone());
                line.clear();
            }
        }
        collected
    });
    let err_handle = std::thread::spawn(move || {
        let mut collected = String::new();
        if let Some(stderr) = stderr {
            use std::io::Read as _;
            let _ = std::io::BufReader::new(stderr).read_to_string(&mut collected);
        }
        collected
    });
    let deadline = Instant::now() + budget;
    while let Some(wait) = deadline
        .checked_duration_since(Instant::now())
        .filter(|wait| !wait.is_zero())
    {
        match rx.recv_timeout(wait) {
            Ok(line) => {
                if enough(&line) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let out = out_handle.join().unwrap_or_default();
    let err = err_handle.join().unwrap_or_default();
    Ok((out, err))
}

#[cfg(test)]
mod tests {
    /// A member's instance name from `dns-sd -B _alab._tcp local.` and its fleet port from
    /// `dns-sd -L`: active `_alab._tcp` instances only, each `host:port` once, a bad port or host
    /// left out.
    #[test]
    fn a_resolved_member_is_its_host_and_fleet_port() {
        let browse = "Browsing for _alab._tcp.local.\n\
            12:00:00.000  Add        3  14 local.               _alab._tcp.          alab-gone\n\
            12:00:00.001  Add        3  14 local.               _alab._tcp.          alab-1a2b3c4d\n\
            12:00:00.002  Add        2  15 local.               _alab._tcp.          alab-9f8e7d6c\n\
            12:00:00.003  Add        2  14 local.               _alab-vault._tcp.    alab-secrets\n\
            12:00:00.004  Rmv        0  14 local.               _alab._tcp.          alab-gone\n";
        assert_eq!(
            super::parse_browse_members(browse),
            ["alab-1a2b3c4d", "alab-9f8e7d6c"]
        );

        let text = "Lookup alab-1a2b3c4d._alab._tcp.local\n\
            12:00:00.000  alab-1a2b3c4d._alab._tcp.local. can be reached at m22.local.:7100 (interface 14)\n\
             v=1 role=member fleet=7100 window=1\n\
            12:00:00.001  alab-1a2b3c4d._alab._tcp.local. can be reached at M22.local.:7100 (interface 15)\n\
            12:00:00.002  x._alab._tcp.local. can be reached at m7.local.:0 (interface 14)\n\
            12:00:00.003  y._alab._tcp.local. can be reached at bad host.local.:7100 (interface 14)\n";
        assert_eq!(super::parse_reached(text), ["m22.local:7100"]);
    }

    /// A joiner's browse keeps every member of a lab of 40 Macs, so each one is asked and can
    /// show the row a person presses Allow on.
    /// NEGATIVE CONTROL: the cut at `MAX_CANDIDATES` (16) this browse had leaves 24 of them
    /// never asked.
    #[test]
    fn a_joiners_browse_keeps_every_member_of_a_lab_of_forty() {
        let mut browse = String::from("Browsing for _alab._tcp.local.\n");
        for n in 0..40 {
            browse.push_str(&format!(
                "12:00:00.000  Add        3  14 local.               _alab._tcp.          alab-{n:08x}\n"
            ));
        }
        let heard = super::parse_browse_members(&browse);
        assert_eq!(heard.len(), 40, "{heard:?}");
        assert_eq!(heard[39], "alab-00000027");
        for n in 40..super::MAX_MEMBERS + 10 {
            browse.push_str(&format!(
                "12:00:00.000  Add        3  14 local.               _alab._tcp.          alab-{n:08x}\n"
            ));
        }
        assert_eq!(
            super::parse_browse_members(&browse).len(),
            super::MAX_MEMBERS,
            "a flood of instances stays bounded"
        );
    }

    use super::*;
    use crate::envelope;

    /// `alab-secrets` and its Bonjour renames are holders; any other name, including one from
    /// the LAN whose multibyte character sits across the cut after `alab-secrets`' length, is
    /// not, and reading it never panics (a panic stopped vault-tend on every server that heard it).
    /// NEGATIVE CONTROL: `split_at` in place of `split_at_checked` panics on the third name.
    #[test]
    fn only_alab_secrets_and_its_renames_are_holders_and_no_name_panics() {
        let alab = crate::alab_advertised();
        assert_eq!(alab, "alab-secrets", "the advertised holder name moved");
        assert!(is_alab_holder_instance("alab-secrets"));
        assert!(is_alab_holder_instance("ALAB-Secrets (2)"));
        assert!(is_alab_holder_instance(" alab-secrets (17) "));
        // `alab-secret` is 11 bytes, then a 2-byte `é` covers bytes 11 and 12.
        for name in [
            "alab-secreté (22)",
            "alab-secret€€ (3)",
            "ålab-secrets (2)",
            "alab-secrets (x)",
            "alab-secrets ()",
            "alab-secrets(2)",
            "alab-secrets (2",
            "m22",
            "",
        ] {
            assert!(!is_alab_holder_instance(name), "{name:?} read as a holder");
        }
    }

    /// A standing browse rings its bell for each vault server it had not heard, or heard on
    /// another interface: once per Add. One heard already, one Removed and not back, and this
    /// process's own instance ring nothing. It keeps what is heard now.
    /// NEGATIVE CONTROL: a watch that keeps rows and rings nothing leaves every nap to its timer.
    #[test]
    fn a_standing_browse_rings_for_each_server_it_had_not_heard() {
        let bell = Arc::new(Bell::default());
        let watch = Watch::idle(&bell, &["m22"]);
        let row = |kind: &str, interface: u32, name: &str| {
            format!(
                "8:30:01.200  {kind}        3   {interface} local.               _alab-vault._tcp.    {name}"
            )
        };
        let rung = || bell.nap(Duration::ZERO);
        watch.feed("Browsing for _alab-vault._tcp.local.");
        assert_eq!(rung(), Wake::Timer, "no row");
        watch.feed(&row("Add", 4, "m3"));
        assert_eq!(rung(), Wake::Browse, "a server not heard before");
        watch.feed(&row("Add", 4, "m3"));
        assert_eq!(rung(), Wake::Timer, "heard already");
        watch.feed(&row("Add", 9, "m3"));
        assert_eq!(rung(), Wake::Browse, "heard on another interface");
        watch.feed(&row("Add", 4, "m22"));
        assert_eq!(rung(), Wake::Timer, "this process's own");
        watch.feed(&row("Rmv", 4, "m3"));
        watch.feed(&row("Rmv", 9, "m3"));
        assert_eq!(rung(), Wake::Timer, "a removal");
        assert_eq!(watch.heard(), ["m22"]);
        watch.feed(&row("Add", 4, "m3"));
        assert_eq!(rung(), Wake::Browse, "back after it left");
        assert_eq!(watch.heard(), ["m22", "m3"]);
        assert!(
            watch.found(Duration::from_secs(1)).is_empty(),
            "a test resolves nothing"
        );
    }

    /// The node job's service.
    const ALAB_TCP: &str = "_alab._tcp";

    /// One `dns-sd -B _alab._tcp local.` row as the standing browse feeds it, newline and all.
    fn alab_row(kind: &str, interface: u32, name: &str) -> String {
        format!(
            "16:02:11.123  {kind}        3   {interface} local.               _alab._tcp.          {name}\n"
        )
    }

    /// A browse its process runs itself feeds the watch as a child's rows do: each start forgets
    /// what the last one heard and opens a settle window, and after it a member the watch did
    /// not hold rings once, this process's own instance and a nameless event nothing.
    /// NEGATIVE CONTROL: a start that keeps what the last browse heard still holds a member that
    /// went while no browse ran, so its return rings nothing.
    #[test]
    fn a_browse_its_process_runs_feeds_the_watch_as_a_childs_rows_do() {
        let bell = Arc::new(Bell::default());
        let rung = || bell.nap(Duration::ZERO);
        let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
        watch.began_browse();
        watch.hear(true, 4, "alab-a1b2c3d4");
        assert_eq!(rung(), Wake::Timer, "heard in the settle window");
        watch.child_began(SETTLE);
        watch.hear(true, 4, "alab-c3d4e5f6");
        assert_eq!(rung(), Wake::Browse, "a member it did not hold");
        watch.hear(true, 4, "alab-0000a0a0");
        watch.hear(true, 4, "");
        assert_eq!(rung(), Wake::Timer, "its own, and no name");
        assert_eq!(
            watch.heard(),
            ["alab-a1b2c3d4", "alab-c3d4e5f6", "alab-0000a0a0"]
        );
        // The connection ended and began again.
        watch.began_browse();
        assert!(watch.heard().is_empty(), "{:?}", watch.heard());
        watch.child_began(SETTLE);
        watch.hear(true, 4, "alab-c3d4e5f6");
        assert_eq!(rung(), Wake::Browse, "heard again on the new connection");
    }

    /// An `_alab._tcp` watch holds what a browse child hears in its settle window as its
    /// baseline, ringing nothing for it. After it, a member it did not hold rings once; one it
    /// holds, another service's row and this process's own instance ring nothing; and a member
    /// that left (the `Rmv` of its last interface) and came back rings again, unless it came back
    /// within [`REREGISTERING_WITHIN`] (re-registering). It holds at most
    /// [`MOST_HELD`] members. The vault watch has no settle window and hears no `_alab._tcp` row.
    /// NEGATIVE CONTROLS: a watch with no baseline rings in the settle window; one that keeps a
    /// row's newline in the name rings for its own instance; one that keeps another service's
    /// row rings for it; one that keeps a removed member still holds it; one with no cap holds
    /// a 257th; one that gives the vault watch the settle window misses its server; one that
    /// passes no member over as re-registering rings for it.
    #[test]
    fn an_alab_tcp_watch_rings_for_a_member_it_did_not_hold_once_its_child_settled() {
        let bell = Arc::new(Bell::default());
        let rung = || bell.nap(Duration::ZERO);
        let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
        watch.feed("Browsing for _alab._tcp.local.\n");
        watch.child_began(Duration::ZERO);
        watch.feed(&alab_row("Add", 4, "alab-a1b2c3d4"));
        assert_eq!(rung(), Wake::Timer, "heard in the settle window");
        assert_eq!(watch.heard(), ["alab-a1b2c3d4"], "a baseline row is held");
        watch.child_began(SETTLE);
        watch.feed(&alab_row("Add", 4, "alab-a1b2c3d4"));
        assert_eq!(rung(), Wake::Timer, "held");
        watch.feed(&alab_row("Add", 4, "alab-c3d4e5f6"));
        assert_eq!(rung(), Wake::Browse, "a member it did not hold");
        watch.feed(&alab_row("Add", 4, "alab-c3d4e5f6"));
        assert_eq!(rung(), Wake::Timer, "held since");
        watch
            .feed("16:02:11.123  Add        3   4 local.               _alab-vault._tcp.    m22\n");
        assert_eq!(rung(), Wake::Timer, "another service");
        watch.feed(&alab_row("Add", 4, "alab-0000a0a0"));
        assert_eq!(rung(), Wake::Timer, "this process's own");
        let left = Instant::now();
        watch.hear_at(false, 4, "alab-c3d4e5f6", left);
        assert_eq!(rung(), Wake::Timer, "a removal");
        assert_eq!(watch.heard(), ["alab-a1b2c3d4", "alab-0000a0a0"]);
        watch.hear_at(true, 4, "alab-c3d4e5f6", left + REREGISTERING_WITHIN);
        assert_eq!(rung(), Wake::Timer, "re-registering");
        let again = left + REREGISTERING_WITHIN;
        watch.hear_at(false, 4, "alab-c3d4e5f6", again);
        watch.hear_at(
            true,
            4,
            "alab-c3d4e5f6",
            again + REREGISTERING_WITHIN + Duration::from_secs(1),
        );
        assert_eq!(rung(), Wake::Browse, "back after it left");

        let crowd = Watch::idle_for(ALAB_TCP, &bell, &[]);
        for n in 0..MOST_HELD {
            crowd.feed(&alab_row("Add", 4, &format!("alab-{n:08x}")));
        }
        assert_eq!(rung(), Wake::Browse);
        crowd.feed(&alab_row("Add", 4, "alab-ffffffff"));
        assert_eq!(rung(), Wake::Timer, "one more than it holds");
        assert_eq!(crowd.heard().len(), MOST_HELD);
        crowd.feed(&alab_row("Rmv", 4, "alab-00000000"));
        crowd.feed(&alab_row("Add", 4, "alab-ffffffff"));
        assert_eq!(rung(), Wake::Browse, "room again");

        let vault = Watch::idle(&bell, &[]);
        vault.child_began(Duration::ZERO);
        vault.feed("8:30:01.200  Add        3   4 local.               _alab-vault._tcp.    m3\n");
        assert_eq!(rung(), Wake::Browse, "the vault watch has no settle window");
        vault.feed(&alab_row("Add", 4, "alab-a1b2c3d4"));
        assert_eq!(
            rung(),
            Wake::Timer,
            "the vault watch hears no _alab._tcp row"
        );
        assert_eq!(vault.heard(), ["m3"]);
    }

    /// `adverts` resolves each member the watch holds but this process's own, in first-heard
    /// order, leaving out one that does not answer; a watch with no resolver resolves nothing in
    /// a test build.
    /// NEGATIVE CONTROL: an `adverts` that resolves this process's own instance fails the list.
    #[test]
    fn adverts_resolve_each_member_held_but_this_process_own() {
        let bell = Arc::new(Bell::default());
        let watch = Watch::idle_resolving(ALAB_TCP, &bell, &["alab-0000a0a0"], |name| {
            (name != "alab-deadbeef").then(|| Advert {
                instance: name.to_owned(),
                host: format!("{name}.local"),
                port: 7100,
                txt: Vec::new(),
            })
        });
        for name in [
            "alab-c3d4e5f6",
            "alab-0000a0a0",
            "alab-deadbeef",
            "alab-a1b2c3d4",
        ] {
            watch.feed(&alab_row("Add", 4, name));
        }
        let adverts = watch.adverts(Duration::from_secs(1));
        assert_eq!(
            adverts
                .iter()
                .map(|advert| advert.instance.as_str())
                .collect::<Vec<_>>(),
            ["alab-c3d4e5f6", "alab-a1b2c3d4"]
        );
        let plain = Watch::idle_for(ALAB_TCP, &bell, &[]);
        plain.feed(&alab_row("Add", 4, "alab-c3d4e5f6"));
        assert!(plain.adverts(Duration::from_secs(1)).is_empty());
    }

    /// `parse_advert` reads an `_alab._tcp` resolve as `dns-sd -L` prints it: the host without
    /// its trailing dot, the fleet port it registered and its TXT pairs, for an instance named
    /// as the browse heard it (a space printed as `\032`). It refuses a transcript with no
    /// "can be reached at" line, or with only another instance's.
    /// NEGATIVE CONTROLS: a parse that takes any instance's line reads the other's; one that
    /// leaves `\032` as it is misses the spaced name.
    #[test]
    fn parse_advert_reads_an_alab_tcp_resolve_and_refuses_one_with_no_target() {
        let transcript = "Lookup alab-1a2b3c4d._alab._tcp.local.\n\
                          DATE: ---Tue 28 Oct 2026---\n\
                          16:02:11.456  ...STARTING...\n\
                          16:02:11.789  alab-1a2b3c4d._alab._tcp.local. can be reached at M3.local.:7100 (interface 4) Flags: 1\n\
                          \x20v=1 role=steward fleet=7100 window=1\n\
                          16:02:11.790  alab-1a2b3c4d._alab._tcp.local. can be reached at M3.local.:7100 (interface 9)\n\
                          \x20v=1 role=steward fleet=7100 window=1\n";
        assert_eq!(
            parse_advert("alab-1a2b3c4d", transcript),
            Some(Advert {
                instance: "alab-1a2b3c4d".to_owned(),
                host: "m3.local".to_owned(),
                port: 7100,
                txt: [
                    ("v", "1"),
                    ("role", "steward"),
                    ("fleet", "7100"),
                    ("window", "1")
                ]
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            })
        );
        assert_eq!(
            parse_advert("alab-ffffffff", transcript),
            None,
            "another instance's"
        );

        let spaced = "Lookup m3 (2)._alab._tcp.local.\n\
                      DATE: ---Tue 28 Oct 2026---\n\
                      \x206:21:48.496  ...STARTING...\n\
                      \x206:21:48.497  m3\\032(2)._alab._tcp.local. can be reached at m3-2.local.:7101 (interface 5)\n\
                      \x20v=1 role=member fleet=7101 window=0\n";
        let advert = parse_advert("m3 (2)", spaced).expect("the spaced name's advert");
        assert_eq!((advert.host.as_str(), advert.port), ("m3-2.local", 7101));
        assert_eq!(advert.txt[1], ("role".to_owned(), "member".to_owned()));

        let unanswered = "Lookup alab-1a2b3c4d._alab._tcp.local.\n\
                          DATE: ---Tue 28 Oct 2026---\n\
                          16:02:11.456  ...STARTING...\n";
        assert_eq!(parse_advert("alab-1a2b3c4d", unanswered), None);
        assert_eq!(parse_advert("alab-1a2b3c4d", ""), None);
    }

    #[test]
    fn browse_and_resolve_parse_active_instances_and_order_txt_hints_behind_defaults() {
        let browse = "Browsing for _alab-vault._tcp.local.\n\
                      8:30:01.200  Add        3  15 local.               _alab-vault._tcp.    gone-node\n\
                      8:30:01.201  Add        3   4 local.               _alab-vault._tcp.    alab-secrets\n\
                      8:30:01.202  Add        2   9 local.               _alab-vault._tcp.    m3\n\
                      8:30:01.203  Add        2   4 local.               _alab-vault._tcp.    m7\n\
                      8:30:01.204  Rmv        0  15 local.               _alab-vault._tcp.    gone-node\n";
        assert_eq!(parse_browse(browse), vec!["alab-secrets", "m3", "m7"]);

        let signer = crate::b64::decode_padded(crate::ALAB_SIGNER).expect("signer");
        let signer = <[u8; 32]>::try_from(signer.as_slice()).expect("32 bytes");
        let fp = envelope::fingerprint(&signer);
        let head = "ab".repeat(32);
        let resolve_text = format!(
            "Lookup m3._alab-vault._tcp.local\n\
             8:30:01.210  m3._alab-vault._tcp.local. can be reached at m3.local.:7484 (interface 9)\n \
             path=/v1 vault=alab signer={fp} seq=19 head={head}\n\
             8:30:01.211  m7._alab-vault._tcp.local. can be reached at m7.local.:7484 (interface 4)\n \
             path=/v1\n\
             8:30:01.212  other._alab-vault._tcp.local. can be reached at other.local.:7484 (interface 4)\n \
             path=/v1 vault=team signer={fp} seq=99 head={head}\n"
        );
        let resolved = parse_resolve(&resolve_text, "alab");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].url, "http://m3.local:7484/v1/alab");
        assert_eq!(resolved[0].txt.as_ref().map(|t| t.seq), Some(19));
        assert_eq!(resolved[1].url, "http://m7.local:7484/v1/alab");
        assert_eq!(resolved[1].txt, None);

        let extra = DiscoveredVault {
            url: "http://m9.local:7484/v1/alab".into(),
            txt: resolved[0].txt.clone().map(|mut t| {
                t.seq = 42;
                t
            }),
        };
        let duplicate_default = DiscoveredVault {
            url: crate::ALAB_DEFAULT_URL
                .to_uppercase()
                .replace("V1/ALAB", "v1/alab"),
            txt: resolved[0].txt.clone(),
        };
        let urls = candidate_urls(
            crate::ALAB_DEFAULT_URLS,
            &[
                resolved[1].clone(),
                resolved[0].clone(),
                extra,
                duplicate_default,
            ],
            "alab",
        );
        assert_eq!(
            urls,
            vec![
                crate::ALAB_DEFAULT_URL.to_owned(),
                "http://m9.local:7484/v1/alab".to_owned(),
                "http://m3.local:7484/v1/alab".to_owned(),
                "http://m7.local:7484/v1/alab".to_owned(),
            ]
        );
    }

    /// About 35 Macs serve, so a server hears about 34 others and a member about 35, and each pass
    /// pulls from (or a member's failover asks) 16 of them. Every one heard is among the 16
    /// within three passes, so a server pulls from a laptop that rejoined after its name expired
    /// (heard last everywhere), and records that only its own server holds reach the rest.
    /// NEGATIVE CONTROL: the first `MAX_CANDIDATES` heard (main before this fix), on every pass,
    /// leave 19 of 35 never pulled from.
    #[test]
    fn every_heard_server_is_among_the_sixteen_within_three_passes() {
        let bell = Arc::new(Bell::default());
        let watch = Watch::idle(&bell, &["m00"]);
        for n in 0..36 {
            watch.feed(&format!(
                "8:30:01.200  Add        3   4 local.               _alab-vault._tcp.    m{n:02}"
            ));
        }
        let (mut peers, mut found) = (
            std::collections::BTreeSet::new(),
            std::collections::BTreeSet::new(),
        );
        for _ in 0..3 {
            let named = watch.peer_names();
            assert!(named.len() <= MAX_CANDIDATES, "{named:?}");
            assert!(!named.iter().any(|name| name == "m00"), "its own instance");
            peers.extend(named);
            let named = watch.found_names();
            assert!(named.len() <= MAX_CANDIDATES, "{named:?}");
            found.extend(named);
        }
        assert_eq!(peers.len(), 35, "{peers:?}");
        assert_eq!(found.len(), 36, "{found:?}");
    }
}
