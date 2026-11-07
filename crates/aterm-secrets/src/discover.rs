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
#[cfg(all(target_os = "macos", not(test)))]
use aterm_uds::lan::{self, LanError};
#[cfg(any(test, target_os = "macos"))]
use aterm_uds::lan::{Event, Instance};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

/// Cap on candidate URLs probed in one pass.
pub const MAX_CANDIDATES: usize = 16;

/// Cap on member service instances resolved by one joiner's browse. Repeated calls in one
/// process rotate through a stable larger set without raising the per-attempt limit.
pub const MAX_MEMBERS: usize = 64;

/// The domain every browse and resolve here names: mDNS's own.
#[cfg(all(target_os = "macos", not(test)))]
const DOMAIN: &str = "local.";

/// One `_alab-vault._tcp` service target resolved on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredVault {
    /// Candidate vault URL (`http://<host>:<port>/v1/<vault>`).
    pub url: String,
    /// Optional parsed TXT hint (`vault`, `signer`, `seq`, `head`), never a trust grant.
    pub txt: Option<VaultTxt>,
}

/// Whether `instance` (as a browse heard it) is of `service`: DNS-SD names the type with its
/// trailing dot (`_alab._tcp.`).
#[cfg(any(test, target_os = "macos"))]
fn of_service(instance: &Instance, service: &str) -> bool {
    instance
        .service
        .strip_suffix('.')
        .unwrap_or(&instance.service)
        == service
}

/// The instances of `service` a browse's `events` leave heard, in first-seen order: each
/// [`Event::Added`] on an interface, until the [`Event::Removed`] of the last interface it was
/// heard on.
#[cfg(any(test, target_os = "macos"))]
fn heard_instances(events: impl IntoIterator<Item = Event>, service: &str) -> Vec<String> {
    let mut instances: Vec<(String, Vec<u32>)> = Vec::new();
    for event in events {
        let (add, instance) = match &event {
            Event::Added(instance) => (true, instance),
            Event::Removed(instance) => (false, instance),
            _ => continue,
        };
        if of_service(instance, service) && !instance.name.is_empty() {
            apply_row(&mut instances, add, instance.interface, &instance.name);
        }
    }
    instances
        .into_iter()
        .filter_map(|(name, interfaces)| (!interfaces.is_empty()).then_some(name))
        .collect()
}

/// The environment variable that points [`browse_members`] at a lab's own members service in
/// place of [`MEMBER_SERVICE`]: `lab mac nux --scenario vault-phrase` sets it on its joiner, whose
/// guest hears the members the lab Mac's LAN advertises and must never ask them. Which Bonjour
/// type a Mac browses is no credential (whoever answers still proves who it is), but this build
/// takes only a [`lab_service`] here and fails the browse on any other value
/// ([`member_service`]), so a value it cannot take never falls back to the real members. No
/// install sets it.
pub const LAB_MEMBER_SERVICE_ENV: &str = "ATERM_LAB_MEMBER_SERVICE";

/// Whether `service` is a lab's members service, `_alab-lab-<1 to 6 lowercase letters or
/// digits>._tcp`: a type no ALab member advertises, whose name (`alab-lab-` and the rest) keeps
/// to DNS-SD's 15 characters.
#[must_use]
pub fn lab_service(service: &str) -> bool {
    service
        .strip_prefix("_alab-lab-")
        .and_then(|rest| rest.strip_suffix("._tcp"))
        .is_some_and(|nonce| {
            (1..=6).contains(&nonce.len())
                && nonce
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// The service [`browse_members`] browses, given [`LAB_MEMBER_SERVICE_ENV`]'s value (`None`:
/// unset): [`MEMBER_SERVICE`], or the [`lab_service`] the variable names.
///
/// # Errors
/// The variable is set to anything but a lab service (`_alab._tcp` itself, an empty value and
/// one that is no UTF-8 included): the browse then looks for no member, never for the real ones.
pub fn member_service(value: Option<&std::ffi::OsStr>) -> crate::Result<String> {
    let Some(value) = value else {
        return Ok(MEMBER_SERVICE.to_owned());
    };
    match value.to_str().filter(|v| lab_service(v)) {
        Some(lab) => Ok(lab.to_owned()),
        None => Err(crate::Error::new(format!(
            "{LAB_MEMBER_SERVICE_ENV} names no lab service (`_alab-lab-<1 to 6 lowercase letters \
             or digits>._tcp`), so this Mac browses for no member"
        ))),
    }
}

/// The environment variable that points the client's vault look at a lab's own vault service in
/// place of [`crate::server::SERVICE_TYPE`]: every vault browse the look runs ([`browse_alab`],
/// [`browse_names`], the subscriber's standing [`Watch::start_look`]) browses the type it names,
/// and the look asks no compiled-in [`crate::ALAB_DEFAULT_URLS`] ([`look_defaults`]), so it names
/// only what its lab browse heard. This changes a browse's target, without starting a browse
/// or admitting an automatic default-vault join: a lab identity is still refused before that
/// join looks (A12). As with [`LAB_MEMBER_SERVICE_ENV`], this build takes only a
/// [`lab_service`] here ([`vault_service`]), so a value it cannot take browses for no vault, and
/// the variable set marks the process a lab's ([`crate::Making`]). A vault server's own browse
/// ([`Watch::start`]) and advert keep `_alab-vault._tcp` whatever it says. No install sets it.
pub const LAB_VAULT_SERVICE_ENV: &str = "ATERM_LAB_VAULT_SERVICE";

/// The type a vault browse browses, given [`LAB_VAULT_SERVICE_ENV`]'s value (`None`: unset):
/// [`crate::server::SERVICE_TYPE`], or the [`lab_service`] the variable names.
///
/// # Errors
/// The variable is set to anything but a lab service (`_alab-vault._tcp` itself, an empty value
/// and one that is no UTF-8 included): the browse then looks for no vault, never for the real one.
pub fn vault_service(value: Option<&std::ffi::OsStr>) -> crate::Result<String> {
    let Some(value) = value else {
        return Ok(crate::server::SERVICE_TYPE.to_owned());
    };
    match value.to_str().filter(|v| lab_service(v)) {
        Some(lab) => Ok(lab.to_owned()),
        None => Err(crate::Error::new(format!(
            "{LAB_VAULT_SERVICE_ENV} names no lab service (`_alab-lab-<1 to 6 lowercase letters \
             or digits>._tcp`), so this Mac browses for no vault"
        ))),
    }
}

/// [`vault_service`] of this process's [`LAB_VAULT_SERVICE_ENV`].
fn vault_type() -> crate::Result<String> {
    vault_service(std::env::var_os(LAB_VAULT_SERVICE_ENV).as_deref())
}

/// The compiled-in addresses the vault look asks beside what its browse heard, given
/// [`LAB_VAULT_SERVICE_ENV`]'s value (`None`: unset): [`crate::ALAB_DEFAULT_URLS`], or none at all
/// where the variable is set, whatever it says (a lab's look names only what its lab browse
/// heard, never the live vault by its well-known name).
#[must_use]
pub(crate) fn look_defaults(lab_vault: Option<&std::ffi::OsStr>) -> &'static [&'static str] {
    if lab_vault.is_some() {
        &[]
    } else {
        crate::ALAB_DEFAULT_URLS
    }
}

/// [`look_defaults`] of this process's [`LAB_VAULT_SERVICE_ENV`].
#[must_use]
pub(crate) fn look_default_urls() -> &'static [&'static str] {
    look_defaults(std::env::var_os(LAB_VAULT_SERVICE_ENV).as_deref())
}

/// The `aterm.log` line for the look hearing vault instance `instance` of `service`.
#[cfg(any(test, target_os = "macos"))]
fn vault_heard_line(service: &str, instance: &str) -> String {
    format!("vault look: heard {instance} on {service}")
}

/// The most distinct vault instances one process logs hearing ([`note_vault_heard`]); mDNS is
/// anyone's to write on the LAN, so what it logs is bounded.
#[cfg(any(test, target_os = "macos"))]
const MOST_LOGGED: usize = 64;

/// Whether `instance` of `service` is new to `logged` (then added to it, while it holds fewer
/// than [`MOST_LOGGED`]): each instance is logged once per process, not at every look.
#[cfg(any(test, target_os = "macos"))]
fn first_heard(logged: &mut Vec<(String, String)>, service: &str, instance: &str) -> bool {
    if logged.len() >= MOST_LOGGED
        || logged
            .iter()
            .any(|(s, i)| s == service && i.eq_ignore_ascii_case(instance))
    {
        return false;
    }
    logged.push((service.to_owned(), instance.to_owned()));
    true
}

/// Log to `aterm.log` ([`vault_heard_line`]) each vault instance of `service` in `heard` this
/// process has not logged before.
#[cfg(any(test, target_os = "macos"))]
fn note_vault_heard<'a>(service: &str, heard: impl IntoIterator<Item = &'a str>) {
    static LOGGED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let mut logged = LOGGED.lock().unwrap_or_else(PoisonError::into_inner);
    for instance in heard {
        if first_heard(&mut logged, service, instance) {
            aterm_log::info!("{}", vault_heard_line(service, instance));
        }
    }
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

/// The service every ALab member's fleet port advertises (`aterm_agent::node_service`), which
/// a joining Mac browses to find the members it asks (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §3.3.1).
pub const MEMBER_SERVICE: &str = "_alab._tcp";

/// The host, lowercase without its trailing dot, and the port a service resolve's `event`
/// answers (`m3.local.`, `7484`). `None` for another event, port 0, or a host of other
/// characters than a DNS name's.
#[cfg(any(test, target_os = "macos"))]
fn reached(event: &Event) -> Option<(String, u16)> {
    let Event::Resolved { host, port, .. } = event else {
        return None;
    };
    let host = host.trim_end_matches('.');
    (*port != 0 && plain_host(host)).then(|| (host.to_ascii_lowercase(), *port))
}

/// A resolve's TXT strings that are UTF-8: one that is not names no key aterm reads.
#[cfg(any(test, target_os = "macos"))]
fn txt_strings(event: &Event) -> Vec<&str> {
    match event {
        Event::Resolved { txt, .. } => txt
            .iter()
            .filter_map(|string| std::str::from_utf8(string).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// The `host:port` a member's resolve answers ([`reached`]).
#[cfg(any(test, target_os = "macos"))]
fn member_target(event: &Event) -> Option<String> {
    reached(event).map(|(host, port)| format!("{host}:{port}"))
}

/// The vault `vault` a `_alab-vault._tcp` resolve answers: its URL, and the [`VaultTxt`] hint its
/// TXT carries. `None` for no usable target, or a TXT naming another path than `/v1` or another
/// vault (each key case-insensitive, its first string winning, as [`VaultTxt::from_txt`] reads).
#[cfg(any(test, target_os = "macos"))]
fn resolved_vault(event: &Event, vault: &str) -> Option<DiscoveredVault> {
    let (host, port) = reached(event)?;
    let strings = txt_strings(event);
    let first = |name: &str| {
        strings.iter().find_map(|string| {
            let (key, value) = string.split_once('=')?;
            key.eq_ignore_ascii_case(name).then_some(value)
        })
    };
    if first("path").is_some_and(|path| path != "/v1")
        || first("vault").is_some_and(|named| named != vault)
    {
        return None;
    }
    let txt = VaultTxt::from_txt(strings.iter().copied());
    if txt.as_ref().is_some_and(|txt| txt.vault != vault) {
        return None;
    }
    Some(DiscoveredVault {
        url: format!("http://{host}:{port}/v1/{vault}"),
        txt,
    })
}

/// `instance`'s advert as its resolve's `event` answers it: where it can be reached and its TXT
/// strings as `key=value` pairs, a string with no `=` or an empty key left out.
#[cfg(any(test, target_os = "macos"))]
fn advert_of(instance: &str, event: &Event) -> Option<Advert> {
    let (host, port) = reached(event)?;
    let txt = txt_strings(event)
        .into_iter()
        .filter_map(|string| {
            let (key, value) = string.split_once('=')?;
            (!key.is_empty()).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect();
    Some(Advert {
        instance: instance.to_owned(),
        host,
        port,
        txt,
    })
}

/// [`LanError`] as this crate's error, saying what was asked.
#[cfg(all(target_os = "macos", not(test)))]
fn lan_failed(what: &str, error: &LanError) -> crate::Error {
    crate::Error::new(format!("{what}: {error}"))
}

/// What a browse of `service` hears within `within`, through the LAN broker: the instances it
/// leaves heard ([`heard_instances`]).
///
/// # Errors
/// The broker is unavailable ([`aterm_uds::lan::Unavailable`], in its words) or refused the
/// browse.
#[cfg(all(target_os = "macos", not(test)))]
fn browse_within(service: &str, within: Duration) -> crate::Result<Vec<String>> {
    let what = format!("browsing {service}");
    let mut events = lan::browse(service, DOMAIN).map_err(|error| lan_failed(&what, &error))?;
    let deadline = Instant::now() + within;
    let mut heard = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match events.next_within(left) {
            Ok(Some(event)) => heard.push(event),
            Ok(None) => break,
            Err(error) if heard.is_empty() => return Err(lan_failed(&what, &error)),
            Err(_) => break,
        }
    }
    Ok(heard_instances(heard, service))
}

/// The first answer a resolve of `instance` of `service` hears within `within`, through the LAN
/// broker: `None` when none came in time, or the resolve failed.
#[cfg(all(target_os = "macos", not(test)))]
fn resolve_first(instance: &str, service: &str, within: Duration) -> Option<Event> {
    let mut events = lan::resolve_service(instance, service, DOMAIN).ok()?;
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        match events.next_within(left) {
            Ok(Some(event @ Event::Resolved { .. })) => return Some(event),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

/// What [`browse_members`] says off macOS, which this build browses for members on alone: a
/// member is named by its address instead.
pub const NO_BROWSE: &str = "this Mac cannot browse the network for ALab's members (that needs \
     macOS's Bonjour): name one with `aterm members join --at <host>`";

/// In unit tests: nothing is browsed, and no member is found.
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

/// Browse [`member_service`] on macOS within `budget`, through the LAN broker, and resolve the
/// fleet ports (`host:port`) of at most [`MAX_MEMBERS`] service instances. Successive calls in
/// this process rotate through names in a stable order, including names beyond the first batch.
///
/// # Errors
/// [`LAB_MEMBER_SERVICE_ENV`] names no lab service, the LAN broker is unavailable (its words say
/// what installs it), or it refused the browse.
#[cfg(all(target_os = "macos", not(test)))]
pub fn browse_members(budget: Duration) -> crate::Result<Vec<String>> {
    static FROM: AtomicUsize = AtomicUsize::new(0);
    let started = Instant::now();
    let service = member_service(std::env::var_os(LAB_MEMBER_SERVICE_ENV).as_deref())?;
    let service = service.as_str();
    let instances = rotated_names(browse_within(service, budget / 2)?, &FROM, MAX_MEMBERS);
    let left = budget.saturating_sub(started.elapsed());
    let resolved: Vec<Option<String>> = std::thread::scope(|scope| {
        let resolving: Vec<_> = instances
            .iter()
            .map(|name| {
                scope.spawn(move || {
                    resolve_first(name, service, left).and_then(|event| member_target(&event))
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

/// One instance's advert as a service resolve answers it: where it can be reached and the TXT
/// it carries. The node job's `_alab._tcp` advert is a fleet member's dial hint: its port is the
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

impl Advert {
    /// Parse the mesh protocol's node hint. This never proves identity or membership.
    /// Duplicate keys, an invalid principal or a TXT port unlike the SRV port are refused.
    #[must_use]
    pub fn mesh_node(&self) -> Option<&str> {
        let one = |name: &str| {
            let mut values = self.txt.iter().filter(|(key, _)| key == name);
            let value = values.next()?.1.as_str();
            values.next().is_none().then_some(value)
        };
        if one("v")? != "2" || one("port")?.parse::<u16>().ok()? != self.port || self.port == 0 {
            return None;
        }
        let node = one("id")?;
        let principal = node.strip_prefix("n-")?;
        (principal.len() == 16
            && principal
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then_some(node)
    }
}

#[cfg(test)]
mod mesh_hint_tests {
    use super::*;

    #[test]
    fn mesh_txt_is_only_a_well_formed_dial_hint() {
        let mut advert = Advert {
            instance: "opaque".into(),
            host: "peer.local".into(),
            port: 7100,
            txt: vec![
                ("v".into(), "2".into()),
                ("id".into(), "n-0123456789abcdef".into()),
                ("port".into(), "7100".into()),
            ],
        };
        assert_eq!(advert.mesh_node(), Some("n-0123456789abcdef"));
        advert.txt.push(("id".into(), "n-fedcba9876543210".into()));
        assert_eq!(advert.mesh_node(), None);
        advert.txt.pop();
        advert.port = 7101;
        assert_eq!(advert.mesh_node(), None);
        advert.port = 7100;
        advert.txt[0].1 = "1".into();
        assert_eq!(advert.mesh_node(), None);
        advert.txt[0].1 = "2".into();
        advert.txt[1].1 = "arbitrary-host".into();
        assert_eq!(advert.mesh_node(), None);
    }
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

/// Whether `a` and `b` name one server's vault (scheme, host and port, and path), whatever the
/// case of their letters ([`transport::describe`]).
pub(crate) fn same_endpoint(a: &str, b: &str) -> bool {
    transport::describe(a).eq_ignore_ascii_case(&transport::describe(b))
}

#[cfg(any(test, target_os = "macos"))]
fn plain_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Browse `_alab-vault._tcp` on macOS within `budget` and resolve advertised targets for `vault`.
///
/// In unit tests and off macOS this asks nothing of the network and returns an empty list; tests
/// exercise discovery and candidate selection deterministically through [`heard_instances`],
/// [`resolved_vault`], [`candidate_urls`] and `client::probe_candidates_with`.
#[cfg(any(test, not(target_os = "macos")))]
pub fn browse_alab(_budget: std::time::Duration) -> crate::Result<Vec<DiscoveredVault>> {
    Ok(Vec::new())
}

/// Browse `_alab-vault._tcp` (or the lab vault service [`LAB_VAULT_SERVICE_ENV`] names) on
/// macOS within `budget`, through the LAN broker, and resolve advertised targets for `alab`.
/// Successive browses in this process rotate through the names in stable name order, reserving
/// space for the compiled-in defaults the look asks ([`look_default_urls`]) in
/// [`candidate_urls`].
///
/// # Errors
/// As [`browse_names`].
#[cfg(all(target_os = "macos", not(test)))]
pub fn browse_alab(budget: Duration) -> crate::Result<Vec<DiscoveredVault>> {
    static FROM: AtomicUsize = AtomicUsize::new(0);
    if budget.is_zero() {
        return Ok(Vec::new());
    }
    let started = Instant::now();
    let heard = browse_names(budget)?;
    let left = budget.saturating_sub(started.elapsed());
    if left < Duration::from_millis(40) {
        return Ok(Vec::new());
    }
    let instances = alab_candidate_names(heard, &FROM);
    if instances.is_empty() {
        return Ok(Vec::new());
    }
    Ok(resolve_all(&instances, left))
}

/// In tests and off macOS, nothing is browsed.
#[cfg(any(test, not(target_os = "macos")))]
pub(crate) fn browse_names(_budget: Duration) -> crate::Result<Vec<String>> {
    Ok(Vec::new())
}

/// Hear names of the vault's type ([`vault_type`]) without resolving them, so a caller can
/// reserve its URL slots first, logging each instance heard ([`note_vault_heard`]).
///
/// # Errors
/// [`LAB_VAULT_SERVICE_ENV`] names no lab service, the LAN broker is unavailable (its words say
/// what installs it), or it refused the browse.
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) fn browse_names(budget: Duration) -> crate::Result<Vec<String>> {
    if budget.is_zero() {
        return Ok(Vec::new());
    }
    let service = vault_type()?;
    let browse_budget = (budget / 4).clamp(Duration::from_millis(50), Duration::from_millis(250));
    let heard = browse_within(&service, browse_budget)?;
    note_vault_heard(&service, heard.iter().map(String::as_str));
    Ok(heard)
}

/// Resolve each of `instances` of the vault's type ([`vault_type`]; none where it names no lab
/// service), all at once within `budget`, through the LAN broker: the first answer of each, as
/// `read` takes it.
#[cfg(all(target_os = "macos", not(test)))]
fn resolve_each_vault(
    instances: &[String],
    budget: Duration,
    read: impl Fn(&Event) -> Option<DiscoveredVault> + Sync,
) -> Vec<DiscoveredVault> {
    let Ok(service) = vault_type() else {
        return Vec::new();
    };
    let service = service.as_str();
    let resolve_budget = (budget / 3).clamp(Duration::from_millis(40), Duration::from_millis(250));
    let read = &read;
    std::thread::scope(|scope| {
        let handles: Vec<_> = instances
            .iter()
            .map(|name| {
                scope.spawn(move || {
                    resolve_first(name, service, resolve_budget).and_then(|event| read(&event))
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap_or_default())
            .collect()
    })
}

/// Resolve each of `instances` within `budget`, all at once, into the targets they advertise for
/// `alab`, one per endpoint (the highest TXT `seq` kept).
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) fn resolve_all(instances: &[String], budget: Duration) -> Vec<DiscoveredVault> {
    let resolved = resolve_each_vault(instances, budget, |event| {
        resolved_vault(event, crate::ALAB_VAULT)
    });
    let mut merged: Vec<DiscoveredVault> = Vec::new();
    for item in resolved {
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

/// A test build, or one off macOS, resolves nothing.
#[cfg(any(test, not(target_os = "macos")))]
pub(crate) fn resolve_all(_instances: &[String], _budget: Duration) -> Vec<DiscoveredVault> {
    Vec::new()
}

/// Resolve each `alab-secrets` holder of `instances` within `budget` without collapsing
/// equal-URL collisions (`DESIGN-fluid-swarm` §4.1 F7: two holders after a heal both resolve
/// `alab-secrets.local:7484`, distinguished by `label=` in their TXT); an exact repeat is kept
/// once.
#[cfg(all(target_os = "macos", not(test)))]
fn resolve_alab_holders(instances: &[String], budget: Duration) -> Vec<DiscoveredVault> {
    let mut out = Vec::new();
    for item in resolve_each_vault(instances, budget, |event| {
        resolved_vault(event, crate::ALAB_VAULT)
    }) {
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

/// Resolve each of `instances` of `service`, at most [`MAX_CANDIDATES`] at once, all within
/// `budget`, through the LAN broker, into the adverts they answer with, in the order of
/// `instances`. One that does not answer before its share of the budget ends is left out, and
/// none starts once less than 40 ms of it is left.
#[cfg(all(target_os = "macos", not(test)))]
fn resolve_adverts(service: &str, instances: &[String], budget: Duration) -> Vec<Advert> {
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
                        if let Some(advert) = resolve_first(name, service, each.min(left))
                            .and_then(|event| advert_of(name, &event))
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

/// A test build, or one off macOS, resolves nothing.
#[cfg(any(test, not(target_os = "macos")))]
fn resolve_adverts(_service: &str, _instances: &[String], _budget: Duration) -> Vec<Advert> {
    Vec::new()
}

/// How long a browse's first events are its baseline on a watch of a service other than the
/// vault's (the node job's `_alab._tcp`): what it hears then was there before this process
/// looked, so it is held but rings nothing.
const SETTLE: Duration = Duration::from_secs(30);

/// The most instances a watch of a service other than the vault's holds: one more is neither
/// held nor rung for (mDNS is anyone's to write on the LAN).
const MOST_HELD: usize = 256;

/// How long every member but this process's own must go unheard, on a watch of a service other
/// than the vault's, for the next one heard to be a move: this Mac back on a LAN is one, a
/// peer's comings and goings are none. Each process names its advert afresh
/// (`aterm_agent::node_service::random_alab_instance_name`), so a peer's window or node job
/// starting again is a member never heard, and among some 40 Macs one comes or goes every half
/// minute: counted a member at a time, the browse ran the window's converge pass 1,138 times in
/// two days, at its move source's rest floor, where route changes ran it 52 times (0.155.0,
/// 2026-11-03 to 05).
pub const SILENCE: Duration = Duration::from_secs(2 * 60);

/// A watch of `service`'s settle window and the most instances it holds. The vault's watch rings
/// from a browse's first event and holds every instance it hears, as it always has; a watch
/// of another service takes [`SETTLE`] and [`MOST_HELD`].
fn holding(service: &str) -> (Duration, usize) {
    if service == crate::server::SERVICE_TYPE {
        (Duration::ZERO, usize::MAX)
    } else {
        (SETTLE, MOST_HELD)
    }
}

/// A standing browse of `<service> local.` through the LAN broker
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.2):
/// the vault's `_alab-vault._tcp`, beside the bounded [`browse_alab`] ([`Watch::start`]), or the
/// node job's `_alab._tcp`, whose adverts are the fleet's dial hints ([`Watch::start_for`],
/// [`Watch::adverts`]). It keeps the browse running, starting it again when it ends, and keeps
/// the instances it hears, each forgotten at the removal from the last interface it was heard on.
/// The vault's watch rings its bell with [`Wake::Browse`] at each add of an instance, or of one
/// on an interface it was not heard on: a server that came back, or a new one. Another service's
/// watch rings only at the add of a member heard once every member but this process's own was
/// unheard for longer than [`SILENCE`], or before it held any: this Mac back on a LAN, not a
/// peer's comings and goings. Adds of this process's own instances ring nothing, and nor do those
/// a browse hears within the watch's settle window. Dropping it ends the browse.
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
    /// The service watched (`_alab-vault._tcp`, `_alab._tcp`).
    service: &'static str,
    /// The type its browse browses and its resolves name: the service, or for the look's vault
    /// watch ([`Watch::start_look`]) the type [`vault_type`] names; `None` (no browse runs) where
    /// [`LAB_VAULT_SERVICE_ENV`] names no lab service.
    browsed: Option<String>,
    /// Whether it is the client look's vault watch ([`Watch::start_look`]), which logs each
    /// instance it hears ([`note_vault_heard`]).
    look: bool,
    /// How long a browse's first events are its baseline ([`holding`]).
    settle: Duration,
    /// The most instances held ([`holding`]).
    most: usize,
    /// Each instance heard, with the interfaces it is heard on.
    instances: Mutex<Vec<(String, Vec<u32>)>>,
    /// When the running browse began, and how long before that it is taken to have
    /// begun (a test's seam, `Watch::child_began`); `None` while none has (an idle watch).
    began: Mutex<Option<(Instant, Duration)>>,
    bell: Weak<Bell>,
    /// This process's own instances.
    own: Mutex<Vec<String>>,
    /// The last event at which the watch held a member but this process's own; `None` while it
    /// has held none since it began or forgot ([`Watched::forget`]). Kept only on a watch of a
    /// service other than the vault's.
    last_held: Mutex<Option<Instant>>,
    /// Where the next [`Watch::found`] starts its cut of what is heard ([`rotated`]).
    found_from: AtomicUsize,
    /// Where the next [`Watch::peer_vaults`] starts its cut of what is heard ([`rotated`]).
    peers_from: AtomicUsize,
    /// Where the next [`Watch::alab_holders`] starts its own cut of eligible holders.
    holders_from: AtomicUsize,
    closed: AtomicBool,
    /// What [`Watch::adverts`] resolves through in place of a service resolve, a test's.
    #[cfg(any(test, debug_assertions))]
    resolve: Option<Resolver>,
}

impl Watch {
    /// Start a standing `_alab-vault._tcp` browse that rings `bell` ([`Watch::start_for`]): a
    /// vault server's, which [`LAB_VAULT_SERVICE_ENV`] does not move.
    #[must_use]
    pub fn start(bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch::start_for(crate::server::SERVICE_TYPE, bell, own)
    }

    /// Start the client look's standing vault browse that rings `bell`: [`Watch::start`], whose
    /// browse browses the type [`vault_type`] names (the lab vault service
    /// [`LAB_VAULT_SERVICE_ENV`] names in a lab's process), and none at all where that variable
    /// names no lab service.
    #[must_use]
    pub fn start_look(bell: &Arc<Bell>, own: &[&str]) -> Watch {
        let mut watched = Watched::new(crate::server::SERVICE_TYPE, vault_type().ok(), bell, own);
        watched.look = true;
        Watch::running(watched)
    }

    /// Start a standing browse of `service` that rings `bell`. `own` names this process's own
    /// instances. A watch of the vault's `_alab-vault._tcp` rings from each browse's first event
    /// and holds every instance it hears; a watch of another service (the node job's
    /// `_alab._tcp`) holds what a browse hears in its first 30 s as its baseline, ringing
    /// nothing for it, holds at most 256 instances, and rings only for a member heard after
    /// every member's [`SILENCE`]. The browse goes through the LAN broker,
    /// and waits for it while it is unavailable. In a test build, and off macOS, no browse runs:
    /// the watch is [`Watch::idle_for`].
    #[must_use]
    pub fn start_for(service: &'static str, bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch::running(Watched::new(service, Some(service.to_owned()), bell, own))
    }

    /// A watch of `watched` whose standing browse runs on its own thread on macOS outside a test
    /// build ([`browse_standing`]); elsewhere, idle.
    fn running(watched: Watched) -> Watch {
        let watch = Watch {
            watched: Arc::new(watched),
        };
        #[cfg(all(target_os = "macos", not(test)))]
        {
            let service = watch.watched.service;
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

    /// A watch of `service` that runs no browse: what it hears reaches it only through
    /// [`Watch::hear`] (or [`Watch::feed`]), and with no browse begun, none of it is a baseline.
    #[must_use]
    pub fn idle_for(service: &'static str, bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch {
            watched: Arc::new(Watched::new(service, Some(service.to_owned()), bell, own)),
        }
    }

    /// Seam: [`Watch::idle_for`] whose [`Watch::adverts`] resolves each instance with `resolve`
    /// in place of a service resolve, for a test that feeds the watch and reads its dial hints.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn idle_resolving(
        service: &'static str,
        bell: &Arc<Bell>,
        own: &[&str],
        resolve: impl Fn(&str) -> Option<Advert> + Send + Sync + 'static,
    ) -> Watch {
        let mut watched = Watched::new(service, Some(service.to_owned()), bell, own);
        watched.resolve = Some(Box::new(resolve));
        Watch {
            watched: Arc::new(watched),
        }
    }

    /// Seam: take it that a browse began `ago` ago, as the standing browse does at each browse it
    /// starts with `ago` zero: what is heard while the browse is younger than the watch's settle
    /// window is its baseline, held but ringing nothing.
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

    /// Take one event of a browse of the watch's service that its process runs itself (a native
    /// Bonjour connection, which [`Watch::idle_for`] leaves to it): instance `name` added
    /// (`added`) or removed on `interface`, as the standing browse takes the broker's events.
    pub fn hear(&self, added: bool, interface: u32, name: &str) {
        self.watched.hear(added, interface, name, Instant::now());
    }

    /// Seam: [`Watch::hear`] at `now`, which times every member's silence ([`SILENCE`]); the
    /// settle window still runs on the clock ([`Watch::child_began`]).
    #[cfg(any(test, debug_assertions))]
    pub fn hear_at(&self, added: bool, interface: u32, name: &str, now: Instant) {
        self.watched.hear(added, interface, name, now);
    }

    /// A browse its process runs itself ([`Watch::hear`]) began, or began again after it ended:
    /// what the last one heard is forgotten ([`Watched::forget`]), and what this one hears within
    /// the settle window is its baseline, as at each browse [`Watch::start_for`] starts.
    pub fn began_browse(&self) {
        self.watched.forget();
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

    /// Resolve the instances heard now, reserving space for the compiled-in defaults within
    /// [`MAX_CANDIDATES`] ([`rotated`]: each call takes the next batch), within `budget`: the
    /// targets they advertise for `alab`. A test build resolves nothing, and nor does a watch of
    /// another service than `_alab-vault._tcp`, whose instances are no vault servers.
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
        alab_candidate_names(self.heard(), &self.watched.found_from)
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
                !own_snapshot.iter().any(|own| {
                    !own.eq_ignore_ascii_case(crate::alab_advertised())
                        && own.eq_ignore_ascii_case(name)
                })
            })
            .collect();
        rotated_names(names, &self.watched.peers_from, MAX_CANDIDATES)
    }

    /// Resolve a rotating batch of at most [`MAX_CANDIDATES`] `alab-secrets` /
    /// `alab-secrets (<N>)` instances without collapsing equal-URL collisions
    /// (`DESIGN-fluid-swarm` §4.1 F7). Its cursor is independent of other lookups;
    /// a watch of another service than `_alab-vault._tcp` resolves none.
    #[must_use]
    pub fn alab_holders(&self, budget: Duration) -> Vec<DiscoveredVault> {
        let names = self.holder_names();
        if names.is_empty() {
            return Vec::new();
        }
        resolve_alab_holders(&names, budget)
    }

    fn holder_names(&self) -> Vec<String> {
        if !self.watched.is_vault() {
            return Vec::new();
        }
        let names: Vec<String> = self
            .heard()
            .into_iter()
            .filter(|name| is_alab_holder_instance(name))
            .collect();
        rotated_names(names, &self.watched.holders_from, MAX_CANDIDATES)
    }

    /// Resolve each instance held now but this process's own through the LAN broker, at most
    /// [`MAX_CANDIDATES`] at once, all within `budget`: where each can be reached and the TXT it
    /// carries ([`advert_of`]), in first-heard order. One that does not
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
        let Some(browsed) = self.watched.browsed.as_deref() else {
            return Vec::new();
        };
        resolve_adverts(browsed, &names, budget)
    }
}

/// Rotate only as many ALab instances as fit beside the valid, distinct defaults in the final
/// candidate list. Resolving a full batch first would discard its tail in [`candidate_urls`]
/// on every pass for some fleet sizes, even though the name-selection cursor rotates.
fn alab_candidate_names(names: Vec<String>, from: &AtomicUsize) -> Vec<String> {
    let defaults = candidate_urls(look_default_urls(), &[], crate::ALAB_VAULT);
    rotated_names(names, from, MAX_CANDIDATES - defaults.len())
}

/// Stabilize a name snapshot before advancing a cursor kept across browse restarts.
/// The parser and watch still retain event order for consumers that need it.
fn rotated_names(mut names: Vec<String>, from: &AtomicUsize, limit: usize) -> Vec<String> {
    names.sort_unstable();
    rotated(names, from, limit)
}

/// At most `limit` of `names` (in the supplied order), starting at `from` and moving it on by as
/// many. For a positive limit and a stable list, each is selected within `ceil(len / limit)`
/// calls; a zero limit selects none. Each consumer keeps its own cursor.
pub(crate) fn rotated<T>(mut names: Vec<T>, from: &AtomicUsize, limit: usize) -> Vec<T> {
    if names.len() > limit {
        let start = from.fetch_add(limit, Ordering::Relaxed) % names.len();
        names.rotate_left(start);
        names.truncate(limit);
    }
    names
}

impl Watched {
    /// A watch of `service` whose browse browses `browsed` (none where `None`).
    fn new(
        service: &'static str,
        browsed: Option<String>,
        bell: &Arc<Bell>,
        own: &[&str],
    ) -> Watched {
        let (settle, most) = holding(service);
        Watched {
            service,
            browsed,
            look: false,
            settle,
            most,
            instances: Mutex::new(Vec::new()),
            began: Mutex::new(None),
            bell: Arc::downgrade(bell),
            own: Mutex::new(own.iter().map(|name| (*name).to_owned()).collect()),
            last_held: Mutex::new(None),
            found_from: AtomicUsize::new(0),
            peers_from: AtomicUsize::new(0),
            holders_from: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
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

    /// Whether what is heard now is the running browse's baseline.
    fn baseline(&self) -> bool {
        self.began
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|(at, ago)| ago.saturating_add(at.elapsed()) < self.settle)
    }

    /// Forget what a browse that ended heard: its instances, and when it last held a member. What
    /// the next one hears in its settle window is its baseline.
    fn forget(&self) {
        self.lock().clear();
        *self
            .last_held
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Take one event heard at `now`. The vault's watch rings at each add of an instance, or of one
    /// on an interface it was not heard on. Another service's watch rings only at an add of a
    /// member while it holds none but this process's own and held none within [`SILENCE`] (or
    /// never did): a member never heard, one back, or one heard on another interface beside
    /// members it holds or held lately is a peer coming or going, no move of this Mac's network.
    /// Neither rings in its settle window or for its own.
    fn hear(&self, add: bool, interface: u32, name: &str, now: Instant) {
        if name.is_empty() {
            return;
        }
        let own = self
            .own
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let is_own = |name: &str| own.iter().any(|own| own.eq_ignore_ascii_case(name));
        let holds =
            |instances: &[(String, Vec<u32>)]| instances.iter().any(|(held, _)| !is_own(held));
        let rings = {
            let mut instances = self.lock();
            let held = instances.iter().any(|(held, _)| held == name);
            if add && instances.len() >= self.most && !held {
                return;
            }
            let held_before = holds(&instances);
            let added = apply_row(&mut instances, add, interface, name);
            instances.retain(|(_, interfaces)| !interfaces.is_empty());
            if self.is_vault() {
                added
            } else {
                // Every event, a removal too, says when the watch last held a member.
                let alone = self.alone(held_before, holds(&instances), now);
                add && alone
            }
        };
        if rings
            && !self.baseline()
            && !is_own(name)
            && let Some(bell) = self.bell.upgrade()
        {
            bell.ring(Wake::Browse);
        }
    }

    /// Take one event of a browse of this watch's browsed type, heard at `now`: an add or a
    /// removal ([`Watched::hear`]), the look's add logged ([`note_vault_heard`]); any other
    /// event, or another type's, is none, and so is every event of a watch with no browsed type.
    #[cfg(any(test, target_os = "macos"))]
    fn hear_event(&self, event: &Event, now: Instant) {
        let (add, instance) = match event {
            Event::Added(instance) => (true, instance),
            Event::Removed(instance) => (false, instance),
            _ => return,
        };
        let Some(browsed) = self.browsed.as_deref() else {
            return;
        };
        if of_service(instance, browsed) {
            if add && self.look {
                note_vault_heard(browsed, [instance.name.as_str()]);
            }
            self.hear(add, instance.interface, &instance.name, now);
        }
    }

    /// Whether an event at `now` came while the watch held no member but this process's own
    /// (`before`: it held one as the event came) and had held none within [`SILENCE`], or never
    /// had. An event before or after which it holds one (`after`) is the last it held one at.
    fn alone(&self, before: bool, after: bool, now: Instant) -> bool {
        let mut last_held = self
            .last_held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let alone =
            !before && last_held.is_none_or(|at| now.saturating_duration_since(at) > SILENCE);
        if before || after {
            *last_held = Some(now);
        }
        alone
    }
}

impl Drop for Watch {
    /// The standing browse sees the watch closed within [`LOOK_CLOSED_EVERY`] and ends its
    /// request to the broker.
    fn drop(&mut self) {
        self.watched.closed.store(true, Ordering::SeqCst);
    }
}

/// The first pause before a standing browse that ended (or could not start for a reason other
/// than the broker's absence) starts again, doubled each time it ends within [`RESTART_LONGEST`]
/// of its start, up to that.
#[cfg(any(test, target_os = "macos"))]
const RESTART_FIRST: Duration = Duration::from_secs(5);
#[cfg(any(test, target_os = "macos"))]
const RESTART_LONGEST: Duration = Duration::from_secs(60);

/// How often a standing browse asks again while the LAN broker is unavailable: its socket is
/// what it waits for (setup's Mac password installs it), and a browse starts as soon as it
/// answers, never on the doubled backoff of a browse that ran and ended.
#[cfg(any(test, target_os = "macos"))]
const BROKER_PAUSE: Duration = Duration::from_secs(2);

/// How long the standing browse waits for an event before it looks whether its watch was
/// dropped.
#[cfg(all(target_os = "macos", not(test)))]
const LOOK_CLOSED_EVERY: Duration = Duration::from_secs(1);

/// How a standing browse's last start went ([`restart_pause`]).
#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Started {
    /// The LAN broker was unavailable: nothing started.
    NoBroker,
    /// It started (and ran this long before it ended), or failed for another reason.
    Ran(Duration),
}

/// How long a standing browse waits before it starts again after `last`, given the backoff
/// `pause` it holds, and the backoff the next end gets: the broker's absence waits
/// [`BROKER_PAUSE`] and leaves the backoff at [`RESTART_FIRST`]; another end waits `pause`
/// (reset to [`RESTART_FIRST`] once a browse ran [`RESTART_LONGEST`]) and doubles it, up to that.
#[cfg(any(test, target_os = "macos"))]
fn restart_pause(last: Started, pause: Duration) -> (Duration, Duration) {
    match last {
        Started::NoBroker => (BROKER_PAUSE, RESTART_FIRST),
        Started::Ran(ran) => {
            let pause = if ran >= RESTART_LONGEST {
                RESTART_FIRST
            } else {
                pause
            };
            (pause, pause.saturating_mul(2).min(RESTART_LONGEST))
        }
    }
}

/// Browse `<type> local.` (the watch's [`Watched::browsed`] type) through the LAN broker for
/// `watched` until the watch is dropped, starting it again whenever it ends (mDNSResponder
/// restarted, or the broker did) or cannot start (no broker yet: [`restart_pause`]), and hand it
/// each event. A browse that ended heard nothing more, so what it heard is forgotten, and the
/// next one's settle window begins with it. A watch with no type to browse browses nothing.
#[cfg(all(target_os = "macos", not(test)))]
fn browse_standing(watched: &Watched) {
    let Some(browsed) = watched.browsed.as_deref() else {
        crate::log_line(&format!(
            "not browsing {}: {LAB_VAULT_SERVICE_ENV} names no lab service",
            watched.service
        ));
        return;
    };
    let mut pause = RESTART_FIRST;
    while !watched.closed.load(Ordering::SeqCst) {
        let started = Instant::now();
        let last = match lan::browse(browsed, DOMAIN) {
            Ok(mut events) => {
                watched.begin(Duration::ZERO);
                while !watched.closed.load(Ordering::SeqCst) {
                    match events.next_within(LOOK_CLOSED_EVERY) {
                        Ok(Some(event)) => watched.hear_event(&event, Instant::now()),
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                watched.forget();
                Started::Ran(started.elapsed())
            }
            Err(LanError::Unavailable(_)) => Started::NoBroker,
            Err(LanError::Io(_)) => Started::Ran(started.elapsed()),
        };
        let (wait, next) = restart_pause(last, pause);
        pause = next;
        let sleep_start = Instant::now();
        while !watched.closed.load(Ordering::SeqCst) && sleep_start.elapsed() < wait {
            let rem = wait.saturating_sub(sleep_start.elapsed());
            std::thread::sleep(Duration::from_secs(1).min(rem));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope;

    /// While the LAN broker is unavailable a standing browse asks again every [`BROKER_PAUSE`],
    /// however long it has waited, and the first browse that then ends waits [`RESTART_FIRST`];
    /// a browse that ran and ended backs off doubling, up to [`RESTART_LONGEST`], and from the
    /// start again once one ran that long. Negative control: the broker's absence on the
    /// doubled backoff waits 5, 10, 20, 40 and 60 s.
    #[test]
    fn a_standing_browse_waits_for_the_broker_on_a_short_cadence() {
        let mut pause = RESTART_FIRST;
        for _ in 0..8 {
            let (wait, next) = restart_pause(Started::NoBroker, pause);
            assert_eq!(wait, BROKER_PAUSE);
            pause = next;
        }
        let quick = Started::Ran(Duration::from_secs(1));
        let mut waits = Vec::new();
        for _ in 0..6 {
            let (wait, next) = restart_pause(quick, pause);
            waits.push(wait.as_secs());
            pause = next;
        }
        assert_eq!(waits, [5, 10, 20, 40, 60, 60]);
        let (wait, _) = restart_pause(Started::Ran(RESTART_LONGEST), pause);
        assert_eq!(wait, RESTART_FIRST);
    }

    /// A browse's add of `name` of `service` on `interface`, as the broker streams it.
    fn added(service: &str, interface: u32, name: &str) -> Event {
        Event::Added(Instance {
            name: name.to_owned(),
            service: format!("{service}."),
            domain: "local.".to_owned(),
            interface,
        })
    }

    /// A browse's removal of `name` of `service` from `interface`.
    fn removed(service: &str, interface: u32, name: &str) -> Event {
        let Event::Added(instance) = added(service, interface, name) else {
            unreachable!("added makes an add")
        };
        Event::Removed(instance)
    }

    /// A service resolve's answer: `host` on `port` with `txt`'s strings.
    fn resolved(host: &str, port: u16, txt: &[&str]) -> Event {
        Event::Resolved {
            host: host.to_owned(),
            port,
            txt: txt
                .iter()
                .map(|string| string.as_bytes().to_vec())
                .collect(),
            interface: 4,
        }
    }

    /// A member's instance name from a browse of `_alab._tcp` and its fleet port from its
    /// resolve: active `_alab._tcp` instances only, the host lowercase without its dot, and a bad
    /// port or host left out.
    #[test]
    fn a_resolved_member_is_its_host_and_fleet_port() {
        let browse = [
            added(MEMBER_SERVICE, 14, "alab-gone"),
            added(MEMBER_SERVICE, 14, "alab-1a2b3c4d"),
            added(MEMBER_SERVICE, 15, "alab-9f8e7d6c"),
            added("_alab-vault._tcp", 14, "alab-secrets"),
            removed(MEMBER_SERVICE, 14, "alab-gone"),
        ];
        assert_eq!(
            heard_instances(browse, MEMBER_SERVICE),
            ["alab-1a2b3c4d", "alab-9f8e7d6c"]
        );
        assert_eq!(
            member_target(&resolved("M22.local.", 7100, &["v=1"])).as_deref(),
            Some("m22.local:7100")
        );
        assert_eq!(member_target(&resolved("m7.local.", 0, &[])), None);
        assert_eq!(member_target(&resolved("bad host.local.", 7100, &[])), None);
        assert_eq!(member_target(&added(MEMBER_SERVICE, 4, "m7")), None);
    }

    /// `ATERM_LAB_MEMBER_SERVICE` unset browses `_alab._tcp`; set, it names a lab service or the
    /// browse fails, and it never falls back to the real members' service.
    ///
    /// NEGATIVE CONTROL (2026-10-29): `member_service` with its refusal arm returning
    /// `Ok(MEMBER_SERVICE.to_owned())` (a value it cannot take falls back to the real members)
    /// fails here at the first refused value: `"_alab._tcp" fell back to Ok("_alab._tcp")`.
    #[test]
    fn a_lab_member_service_is_taken_and_any_other_value_finds_no_member() {
        use std::ffi::OsStr;
        assert_eq!(super::member_service(None).unwrap(), "_alab._tcp");
        for lab in [
            "_alab-lab-k3x9q2._tcp",
            "_alab-lab-a._tcp",
            "_alab-lab-000000._tcp",
        ] {
            assert!(super::lab_service(lab), "{lab}");
            assert_eq!(super::member_service(Some(OsStr::new(lab))).unwrap(), lab);
        }
        for refused in [
            "_alab._tcp",
            "_alab-vault._tcp",
            "",
            "_alab-lab-._tcp",
            "_alab-lab-k3x9q2x._tcp",
            "_alab-lab-K3X9._tcp",
            "_alab-lab-k3x9._udp",
            "_alab-lab-k3-9._tcp",
            "_alab-lab-k3x9._tcp.",
            " _alab-lab-k3x9._tcp",
            "_alab-nux-k3x9._tcp",
        ] {
            assert!(!super::lab_service(refused), "{refused:?}");
            let said = super::member_service(Some(OsStr::new(refused)));
            assert!(said.is_err(), "{refused:?} fell back to {said:?}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            assert!(
                super::member_service(Some(OsStr::from_bytes(b"_alab-lab-\xff._tcp"))).is_err()
            );
        }
    }

    /// `ATERM_LAB_VAULT_SERVICE` unset, a vault browse browses `_alab-vault._tcp`; set, it names
    /// a lab service or no vault is browsed at all, never the real vault's type.
    ///
    /// NEGATIVE CONTROL: `vault_service` with its refusal arm returning
    /// `Ok(SERVICE_TYPE.to_owned())` fails here at the first refused value.
    #[test]
    fn a_lab_vault_service_is_taken_and_any_other_value_browses_no_vault() {
        use std::ffi::OsStr;
        let vault = crate::server::SERVICE_TYPE;
        let lab = "_alab-lab-k3x9q2._tcp";
        assert_eq!(super::vault_service(None).unwrap(), vault);
        assert_eq!(super::vault_service(Some(OsStr::new(lab))).unwrap(), lab);
        for refused in [vault, "_alab._tcp", "", "_alab-lab-K3X9._tcp"] {
            let said = super::vault_service(Some(OsStr::new(refused)));
            assert!(said.is_err(), "{refused:?} fell back to {said:?}");
        }
    }

    /// `ATERM_LAB_VAULT_SERVICE` set, good value or bad, the look's candidates hold no
    /// compiled-in ALab address: a lab's look names only what its lab browse heard. Unset, the
    /// defaults come first, as ever.
    ///
    /// NEGATIVE CONTROL: `look_defaults` answering `ALAB_DEFAULT_URLS` for a set variable fails
    /// at the first value.
    #[test]
    fn a_lab_look_asks_no_compiled_in_address() {
        use std::ffi::OsStr;
        let heard = DiscoveredVault {
            url: "http://alab-secrets.local:7485/v1/alab".to_owned(),
            txt: None,
        };
        for set in ["_alab-lab-k3x9q2._tcp", crate::server::SERVICE_TYPE, ""] {
            let urls = candidate_urls(
                super::look_defaults(Some(OsStr::new(set))),
                std::slice::from_ref(&heard),
                crate::ALAB_VAULT,
            );
            assert!(
                urls.iter().all(|url| !crate::is_alab_default_url(url)),
                "{set:?}: {urls:?}"
            );
            assert_eq!(urls, std::slice::from_ref(&heard.url), "{set:?}");
        }
        let urls = candidate_urls(
            super::look_defaults(None),
            std::slice::from_ref(&heard),
            crate::ALAB_VAULT,
        );
        assert_eq!(
            urls.first().map(String::as_str),
            Some(crate::ALAB_DEFAULT_URL)
        );
        assert!(urls.contains(&heard.url));
    }

    /// The look logs each vault instance it hears once per process, on a line naming the
    /// instance and the service, and holds at most `MOST_LOGGED` of them.
    ///
    /// NEGATIVE CONTROL: `first_heard` returning `true` for one already logged fails at the
    /// second hearing.
    #[test]
    fn each_vault_instance_heard_is_logged_once() {
        let lab = "_alab-lab-k3x9q2._tcp";
        let line = super::vault_heard_line(lab, "alab-secrets");
        assert!(
            line.contains(lab) && line.contains("alab-secrets"),
            "{line}"
        );
        let mut logged = Vec::new();
        assert!(super::first_heard(&mut logged, lab, "alab-secrets"));
        assert!(!super::first_heard(&mut logged, lab, "ALAB-SECRETS"));
        assert!(super::first_heard(
            &mut logged,
            crate::server::SERVICE_TYPE,
            "alab-secrets"
        ));
        for n in logged.len()..super::MOST_LOGGED {
            assert!(super::first_heard(&mut logged, lab, &format!("m{n}")));
        }
        assert!(!super::first_heard(&mut logged, lab, "one-more"));
    }

    /// Only the client look's watch logs what it hears and takes the lab knob; a vault server's
    /// [`Watch::start`] browses `_alab-vault._tcp`, the type its own advert is registered under.
    ///
    /// NEGATIVE CONTROL: `Watch::start_for` building its watch with `vault_type().ok()` or
    /// `look: true` fails here.
    #[test]
    fn a_servers_vault_watch_is_not_the_look() {
        let bell = Arc::new(Bell::default());
        let server = Watch::start(&bell, &[]);
        assert!(!server.watched.look);
        assert_eq!(
            server.watched.browsed.as_deref(),
            Some(crate::server::SERVICE_TYPE)
        );
        assert!(Watch::start_look(&bell, &[]).watched.look);
    }

    /// The vault's watch, where `ATERM_LAB_VAULT_SERVICE` names a lab service, hears that
    /// service's instances alone; the real vault's type is another type's event to it.
    ///
    /// NEGATIVE CONTROL: `hear_event` matching `Watched::service` in place of `browsed` hears the
    /// real vault and not the lab's.
    #[test]
    fn the_vault_watch_of_a_lab_service_hears_that_service_alone() {
        let lab = "_alab-lab-k3x9q2._tcp";
        let bell = Arc::new(Bell::default());
        let watch = Watch::idle(&bell, &[]);
        let watched = Watched::new(
            crate::server::SERVICE_TYPE,
            Some(lab.to_owned()),
            &bell,
            &[],
        );
        let lab_watch = Watch {
            watched: Arc::new(watched),
        };
        for w in [&watch, &lab_watch] {
            hears(w, &added(crate::server::SERVICE_TYPE, 4, "alab-secrets"));
            hears(w, &added(lab, 4, "alab-secrets (2)"));
        }
        assert_eq!(watch.heard(), ["alab-secrets"]);
        assert_eq!(lab_watch.heard(), ["alab-secrets (2)"]);
    }

    /// A browse of a lab service reads that service's instances alone: a real member heard on the
    /// same browse is no lab member, and a lab member is no real one.
    ///
    /// NEGATIVE CONTROL: `heard_instances` reading `MEMBER_SERVICE` whatever service it is given
    /// fails here: `left: ["alab-1a2b3c4d"]`, `right: ["alab-5e6f7a8b"]`.
    #[test]
    fn a_lab_browse_reads_its_own_service_alone() {
        let browse = [
            added(MEMBER_SERVICE, 14, "alab-1a2b3c4d"),
            added("_alab-lab-k3x9q2._tcp", 6, "alab-5e6f7a8b"),
            added("_alab-lab-zzzzzz._tcp", 6, "alab-00000000"),
        ];
        assert_eq!(
            heard_instances(browse.clone(), "_alab-lab-k3x9q2._tcp"),
            ["alab-5e6f7a8b"]
        );
        assert_eq!(heard_instances(browse, MEMBER_SERVICE), ["alab-1a2b3c4d"]);
    }

    /// A joiner's browse keeps every member of a lab of 40 Macs, so each one is asked; a flood of
    /// instances is asked at most [`MAX_MEMBERS`] at a time.
    /// NEGATIVE CONTROL: the cut at `MAX_CANDIDATES` (16) this browse had leaves 24 of them
    /// never asked.
    #[test]
    fn a_joiners_browse_keeps_every_member_of_a_lab_of_forty() {
        let mut browse: Vec<Event> = (0..40)
            .map(|n| added(MEMBER_SERVICE, 14, &format!("alab-{n:08x}")))
            .collect();
        let heard = heard_instances(browse.clone(), MEMBER_SERVICE);
        assert_eq!(heard.len(), 40, "{heard:?}");
        assert_eq!(heard[39], "alab-00000027");
        assert_eq!(
            rotated_names(heard, &AtomicUsize::new(0), MAX_MEMBERS).len(),
            40
        );
        browse.extend(
            (40..MAX_MEMBERS + 10).map(|n| added(MEMBER_SERVICE, 14, &format!("alab-{n:08x}"))),
        );
        assert_eq!(
            rotated_names(
                heard_instances(browse, MEMBER_SERVICE),
                &AtomicUsize::new(0),
                MAX_MEMBERS
            )
            .len(),
            MAX_MEMBERS,
            "a flood of instances stays bounded"
        );
    }

    #[test]
    fn repeated_member_browses_do_not_exclude_names_beyond_the_first_batch() {
        let names: Vec<_> = (0..MAX_MEMBERS * 2 + 1)
            .map(|n| format!("alab-{n:08x}"))
            .collect();
        let expected: std::collections::BTreeSet<_> = names.iter().cloned().collect();
        for reordered in [false, true] {
            let from = AtomicUsize::new(0);
            let mut selected = std::collections::BTreeSet::new();
            for pass in 0..names.len().div_ceil(MAX_MEMBERS) {
                let mut arrivals = names.clone();
                if reordered {
                    arrivals.rotate_right((pass * MAX_MEMBERS) % names.len());
                }
                let browse = arrivals.iter().map(|name| added(MEMBER_SERVICE, 14, name));
                let heard = heard_instances(browse, MEMBER_SERVICE);
                assert_eq!(heard, arrivals, "the fold keeps the browse's order");
                let batch = rotated_names(heard, &from, MAX_MEMBERS);
                assert!(batch.len() <= MAX_MEMBERS);
                assert!(batch.iter().all(|name| expected.contains(name)));
                selected.extend(batch);
            }
            assert_eq!(selected, expected, "reordered={reordered}");
        }
    }

    #[test]
    fn lab_member_batches_rotate_without_selecting_live_members() {
        let service = "_alab-lab-merge1._tcp";
        let browse: Vec<Event> = (0..MAX_MEMBERS * 2)
            .flat_map(|n| {
                [service, MEMBER_SERVICE].map(move |kind| added(kind, 14, &format!("alab-{n:08x}")))
            })
            .collect();
        let from = AtomicUsize::new(0);
        let batch = |service: &str| {
            rotated_names(heard_instances(browse.clone(), service), &from, MAX_MEMBERS)
        };
        let first = batch(service);
        let second = batch(service);
        assert_eq!(first.len(), MAX_MEMBERS);
        assert_eq!(second.len(), MAX_MEMBERS);
        assert!(first.iter().all(|name| !second.contains(name)));
        assert!(batch("_alab-lab-other._tcp").is_empty());
    }

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
        let rung = || bell.nap(Duration::ZERO);
        assert_eq!(rung(), Wake::Timer, "nothing heard");
        watch.hear(true, 4, "m3");
        assert_eq!(rung(), Wake::Browse, "a server not heard before");
        watch.hear(true, 4, "m3");
        assert_eq!(rung(), Wake::Timer, "heard already");
        watch.hear(true, 9, "m3");
        assert_eq!(rung(), Wake::Browse, "heard on another interface");
        watch.hear(true, 4, "m22");
        assert_eq!(rung(), Wake::Timer, "this process's own");
        watch.hear(true, 9, "M22");
        assert_eq!(rung(), Wake::Timer, "this process's own in uppercase");
        watch.hear(false, 9, "M22");
        watch.hear(false, 4, "m3");
        watch.hear(false, 9, "m3");
        assert_eq!(rung(), Wake::Timer, "a removal");
        assert_eq!(watch.heard(), ["m22"]);
        watch.hear(true, 4, "m3");
        assert_eq!(rung(), Wake::Browse, "back after it left");
        assert_eq!(watch.heard(), ["m22", "m3"]);
        assert!(
            watch.found(Duration::from_secs(1)).is_empty(),
            "a test resolves nothing"
        );
    }

    /// The node job's service.
    const ALAB_TCP: &str = "_alab._tcp";

    /// Have `watch` hear `event` now, as the standing browse hands it the broker's.
    fn hears(watch: &Watch, event: &Event) {
        watch.watched.hear_event(event, Instant::now());
    }

    /// A browse its process runs itself feeds the watch as the standing browse does: each start
    /// forgets what the last one heard and when it last held a member, and opens a settle window.
    /// After it a member heard beside one held, this process's own instance and a nameless event
    /// ring nothing, and a member heard after a settle window that heard none rings once.
    /// NEGATIVE CONTROL: a start that keeps what the last browse heard still holds a member that
    /// went while no browse ran, and one that keeps when the last browse held a member takes the
    /// member as heard beside those: either way its return rings nothing.
    #[test]
    fn a_browse_its_process_runs_feeds_the_watch_as_the_standing_browse_does() {
        let bell = Arc::new(Bell::default());
        let rung = || bell.nap(Duration::ZERO);
        let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
        watch.began_browse();
        watch.hear(true, 4, "alab-a1b2c3d4");
        assert_eq!(rung(), Wake::Timer, "heard in the settle window");
        watch.child_began(SETTLE);
        watch.hear(true, 4, "alab-c3d4e5f6");
        assert_eq!(rung(), Wake::Timer, "beside a member it holds");
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

    /// An `_alab._tcp` watch holds what a browse hears in its settle window as its baseline,
    /// ringing nothing for it, nor for a member heard beside it. With nothing held but its own, a
    /// member rings once; one it holds, one heard beside it, another service's event and this
    /// process's own instance ring nothing, and a removed member is held no more. It holds at
    /// most [`MOST_HELD`] members. The vault watch has no settle window and hears no
    /// `_alab._tcp` event. Members coming and going are
    /// `an_alab_tcp_watch_rings_only_for_a_member_heard_after_every_member_was_silent`'s.
    /// NEGATIVE CONTROLS: a watch with no baseline rings in the settle window; one that keeps
    /// another service's event rings for it; one that keeps a removed member still holds it; one
    /// with no cap holds a 257th; one that gives the vault watch the settle window misses its
    /// server.
    #[test]
    fn an_alab_tcp_watch_holds_a_baseline_and_rings_for_a_member_where_it_held_none() {
        let bell = Arc::new(Bell::default());
        let rung = || bell.nap(Duration::ZERO);
        let settling = Watch::idle_for(ALAB_TCP, &bell, &[]);
        settling.child_began(Duration::ZERO);
        hears(&settling, &added(ALAB_TCP, 4, "alab-a1b2c3d4"));
        assert_eq!(rung(), Wake::Timer, "heard in the settle window");
        assert_eq!(
            settling.heard(),
            ["alab-a1b2c3d4"],
            "a baseline event is held"
        );
        settling.child_began(SETTLE);
        hears(&settling, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
        assert_eq!(rung(), Wake::Timer, "beside a baseline member");

        let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
        watch.child_began(SETTLE);
        hears(&watch, &added(crate::server::SERVICE_TYPE, 4, "m22"));
        assert_eq!(rung(), Wake::Timer, "another service");
        hears(&watch, &added(ALAB_TCP, 4, "alab-0000a0a0"));
        assert_eq!(rung(), Wake::Timer, "this process's own");
        hears(&watch, &added(ALAB_TCP, 5, "ALAB-0000A0A0"));
        assert_eq!(rung(), Wake::Timer, "this process's own (case-insensitive)");
        hears(&watch, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
        assert_eq!(rung(), Wake::Browse, "a member, where it held none");
        hears(&watch, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
        assert_eq!(rung(), Wake::Timer, "held since");
        hears(&watch, &added(ALAB_TCP, 4, "alab-a1b2c3d4"));
        assert_eq!(rung(), Wake::Timer, "beside one it holds");
        assert_eq!(
            watch.peer_names(),
            ["alab-a1b2c3d4", "alab-c3d4e5f6"],
            "own instances excluded from peer_names regardless of case"
        );
        hears(&watch, &removed(ALAB_TCP, 5, "ALAB-0000A0A0"));
        hears(&watch, &removed(ALAB_TCP, 4, "alab-c3d4e5f6"));
        assert_eq!(rung(), Wake::Timer, "a removal");
        assert_eq!(watch.heard(), ["alab-0000a0a0", "alab-a1b2c3d4"]);

        let crowd = Watch::idle_for(ALAB_TCP, &bell, &[]);
        for n in 0..MOST_HELD {
            hears(&crowd, &added(ALAB_TCP, 4, &format!("alab-{n:08x}")));
        }
        assert_eq!(rung(), Wake::Browse);
        hears(&crowd, &added(ALAB_TCP, 4, "alab-ffffffff"));
        assert_eq!(rung(), Wake::Timer, "one more than it holds");
        assert_eq!(crowd.heard().len(), MOST_HELD);
        hears(&crowd, &removed(ALAB_TCP, 4, "alab-00000000"));
        hears(&crowd, &added(ALAB_TCP, 4, "alab-ffffffff"));
        assert_eq!(rung(), Wake::Timer, "beside the members it holds");
        assert_eq!(crowd.heard().len(), MOST_HELD, "room again");
        assert_eq!(
            crowd.heard().last().map(String::as_str),
            Some("alab-ffffffff")
        );

        let vault = Watch::idle(&bell, &[]);
        vault.child_began(Duration::ZERO);
        hears(&vault, &added(crate::server::SERVICE_TYPE, 4, "m3"));
        assert_eq!(rung(), Wake::Browse, "the vault watch has no settle window");
        hears(&vault, &added(ALAB_TCP, 4, "alab-a1b2c3d4"));
        assert_eq!(
            rung(),
            Wake::Timer,
            "the vault watch hears no _alab._tcp event"
        );
        assert_eq!(vault.heard(), ["m3"]);
    }

    /// A peer's comings and goings on `_alab._tcp` are no move of this Mac's network (each
    /// process names its advert afresh, so a peer's window or node job starting again is a name
    /// never heard): while the watch holds a member, one never heard, one back after leaving for
    /// longer than [`SILENCE`], and one heard on another interface ring nothing, over an hour
    /// and a half of such churn. A member heard once every member but this process's own was
    /// silent for longer than [`SILENCE`] (this Mac back on a LAN) rings once, and the next one
    /// heard with it nothing.
    /// NEGATIVE CONTROLS: a member heard after a silence no longer than [`SILENCE`] rings
    /// nothing, and nor does this process's own instance, nor an event in the settle window.
    /// MUTANTS: the rule before this one (each member not held rings unless back within the
    /// silence) fails "never heard"; taking the watch as alone whenever it holds no member, with
    /// no silence, or noting when it held one only at adds fails "back within the silence";
    /// noting it at every event after which it holds one, not at the last member's removal,
    /// fails "within the silence of the last member's removal"; taking it as alone once it heard
    /// no event for the silence, though it holds a member, fails "back after it left"; counting
    /// this process's own instance as a member fails "after every member's silence".
    #[test]
    fn an_alab_tcp_watch_rings_only_for_a_member_heard_after_every_member_was_silent() {
        let secs = Duration::from_secs;
        let silence = SILENCE;
        let bell = Arc::new(Bell::default());
        let rung = || bell.nap(Duration::ZERO);
        let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
        let start = Instant::now();
        watch.child_began(Duration::ZERO);
        watch.hear_at(true, 4, "alab-00000000", start);
        assert_eq!(rung(), Wake::Timer, "heard in the settle window");
        watch.child_began(SETTLE);
        watch.hear_at(true, 4, "alab-0000a0a0", start);
        let mut at = start;
        for i in 1..40_u32 {
            let name = format!("alab-{i:08x}");
            at += secs(31);
            watch.hear_at(true, 4, &name, at);
            assert_eq!(rung(), Wake::Timer, "never heard: {name}");
            watch.hear_at(false, 4, &name, at + secs(1));
            at += silence + secs(2);
            watch.hear_at(true, 4, &name, at);
            assert_eq!(rung(), Wake::Timer, "back after it left: {name}");
            watch.hear_at(true, 5, &name, at);
            assert_eq!(rung(), Wake::Timer, "on another interface: {name}");
        }
        assert!(at - start > secs(90 * 60), "{:?}", at - start);
        assert_eq!(watch.heard().len(), 41, "{:?}", watch.heard());

        let left = at + secs(1);
        for name in watch.heard() {
            for interface in [4, 5] {
                watch.hear_at(false, interface, &name, left);
            }
        }
        assert!(watch.heard().is_empty(), "{:?}", watch.heard());
        watch.hear_at(true, 4, "alab-00000000", left + silence);
        assert_eq!(rung(), Wake::Timer, "back within the silence");
        let left = left + silence + secs(1);
        watch.hear_at(false, 4, "alab-00000000", left);
        watch.hear_at(true, 4, "alab-0badcafe", left + silence);
        assert_eq!(
            rung(),
            Wake::Timer,
            "within the silence of the last member's removal"
        );
        let left = left + silence;
        watch.hear_at(false, 4, "alab-0badcafe", left);
        let back = left + silence + secs(1);
        watch.hear_at(true, 4, "alab-0000a0a0", back);
        assert_eq!(rung(), Wake::Timer, "its own, after every member's silence");
        watch.hear_at(true, 4, "alab-feedface", back);
        assert_eq!(rung(), Wake::Browse, "after every member's silence");
        watch.hear_at(true, 4, "alab-00000000", back);
        assert_eq!(rung(), Wake::Timer, "one heard with it");
        assert_eq!(
            watch.heard(),
            ["alab-0000a0a0", "alab-feedface", "alab-00000000"]
        );
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
            hears(&watch, &added(ALAB_TCP, 4, name));
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
        hears(&plain, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
        assert!(plain.adverts(Duration::from_secs(1)).is_empty());
    }

    /// `advert_of` reads an `_alab._tcp` resolve's answer: the host lowercase without its
    /// trailing dot, the fleet port it registered and its TXT pairs (a string with no `=`, an
    /// empty key or no UTF-8 left out), under the instance name the browse heard. It refuses an
    /// answer with no usable target, and any other event.
    /// NEGATIVE CONTROL: an `advert_of` that keeps a string with no `=` reads `"bare"` as a pair.
    #[test]
    fn advert_of_reads_an_alab_tcp_resolve_and_refuses_one_with_no_target() {
        let mut answer = resolved(
            "M3.local.",
            7100,
            &[
                "v=1",
                "role=steward",
                "bare",
                "=x",
                "fleet=7100",
                "window=1",
            ],
        );
        if let Event::Resolved { txt, .. } = &mut answer {
            txt.push(vec![b'k', b'=', 0xff]);
        }
        assert_eq!(
            advert_of("alab-1a2b3c4d", &answer),
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
        let spaced = advert_of("m3 (2)", &resolved("m3-2.local.", 7101, &["role=member"]))
            .expect("the spaced name's advert");
        assert_eq!(
            (spaced.instance.as_str(), spaced.host.as_str(), spaced.port),
            ("m3 (2)", "m3-2.local", 7101)
        );
        assert_eq!(advert_of("x", &resolved("m3.local.", 0, &[])), None);
        assert_eq!(advert_of("x", &resolved("-m3.local.", 7100, &[])), None);
        assert_eq!(advert_of("x", &added(ALAB_TCP, 4, "x")), None);
    }

    #[test]
    fn browse_and_resolve_read_active_instances_and_order_txt_hints_behind_defaults() {
        let vault = crate::server::SERVICE_TYPE;
        let browse = [
            added(vault, 15, "gone-node"),
            added(vault, 4, "alab-secrets"),
            added(vault, 9, "m3"),
            added(vault, 4, "m7"),
            removed(vault, 15, "gone-node"),
        ];
        assert_eq!(
            heard_instances(browse, vault),
            vec!["alab-secrets", "m3", "m7"]
        );

        let signer = crate::b64::decode_padded(crate::ALAB_SIGNER).expect("signer");
        let signer = <[u8; 32]>::try_from(signer.as_slice()).expect("32 bytes");
        let fp = envelope::fingerprint(&signer);
        let head = "ab".repeat(32);
        let signed = [
            "path=/v1".to_owned(),
            "vault=alab".to_owned(),
            format!("signer={fp}"),
            "seq=19".to_owned(),
            format!("head={head}"),
        ];
        let signed: Vec<&str> = signed.iter().map(String::as_str).collect();
        let resolved_m3 =
            resolved_vault(&resolved("m3.local.", 7484, &signed), "alab").expect("m3's answer");
        assert_eq!(resolved_m3.url, "http://m3.local:7484/v1/alab");
        assert_eq!(resolved_m3.txt.as_ref().map(|t| t.seq), Some(19));
        let resolved_m7 = resolved_vault(&resolved("M7.local.", 7484, &["path=/v1"]), "alab")
            .expect("m7's answer");
        assert_eq!(resolved_m7.url, "http://m7.local:7484/v1/alab");
        assert_eq!(resolved_m7.txt, None);
        let team: Vec<&str> = signed
            .iter()
            .map(|string| {
                if *string == "vault=alab" {
                    "vault=team"
                } else {
                    string
                }
            })
            .collect();
        assert_eq!(
            resolved_vault(&resolved("other.local.", 7484, &team), "alab"),
            None,
            "another vault"
        );
        assert_eq!(
            resolved_vault(&resolved("other.local.", 7484, &["path=/v2"]), "alab"),
            None,
            "another path"
        );
        let resolved = [resolved_m7, resolved_m3];
        let extra = DiscoveredVault {
            url: "http://m9.local:7484/v1/alab".into(),
            txt: resolved[1].txt.clone().map(|mut t| {
                t.seq = 42;
                t
            }),
        };
        let duplicate_default = DiscoveredVault {
            url: crate::ALAB_DEFAULT_URL
                .to_uppercase()
                .replace("V1/ALAB", "v1/alab"),
            txt: resolved[1].txt.clone(),
        };
        let urls = candidate_urls(
            crate::ALAB_DEFAULT_URLS,
            &[
                resolved[0].clone(),
                resolved[1].clone(),
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

    /// About 35 Macs serve, so a server hears about 34 others and a member about 35. Each lookup
    /// selects every stable heard name within three passes, including a laptop that rejoined
    /// after its name expired and was heard last. Selection does not establish a response.
    /// NEGATIVE CONTROL: selecting only the first `MAX_CANDIDATES` heard on every pass would
    /// leave 19 of 35 never selected.
    #[test]
    fn every_heard_server_is_selected_within_three_bounded_passes() {
        let bell = Arc::new(Bell::default());
        let watch = Watch::idle(&bell, &["m00"]);
        for n in 0..36 {
            watch.hear(true, 4, &format!("m{n:02}"));
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

        let alab_watch = Watch::idle(&bell, &["ALAB-SECRETS"]);
        alab_watch.hear(true, 4, "alab-secrets");
        assert_eq!(
            alab_watch.peer_names(),
            vec!["alab-secrets"],
            "alab-secrets remains in peer_names regardless of own-note case"
        );
    }

    #[test]
    fn every_holder_is_queried_within_bounded_passes_independently_of_other_lookups() {
        for count in [0usize, 1, MAX_CANDIDATES, MAX_CANDIDATES + 1, 35] {
            let bell = Arc::new(Bell::default());
            let watch = Watch::idle(&bell, &["alab-secrets"]);
            let mut expected = std::collections::BTreeSet::new();
            for index in 0..count {
                let name = if index == 0 {
                    "alab-secrets".to_owned()
                } else {
                    format!("alab-secrets ({})", index + 1)
                };
                watch.hear(true, 1, &name);
                expected.insert(name);
                watch.hear(true, 1, &format!("ordinary-vault-{index}"));
            }
            let mut queried = std::collections::BTreeSet::new();
            for _ in 0..count.div_ceil(MAX_CANDIDATES).max(1) {
                // These consumers have independent progress, even on the same watch.
                let _ = watch.found_names();
                let _ = watch.found_names();
                let _ = watch.peer_names();
                let names = watch.holder_names();
                assert!(names.len() <= MAX_CANDIDATES);
                assert!(names.iter().all(|name| is_alab_holder_instance(name)));
                queried.extend(names);
            }
            assert_eq!(queried, expected, "holder count {count}");
        }

        let bell = Arc::new(Bell::default());
        let members = Watch::idle_for(MEMBER_SERVICE, &bell, &[]);
        members.hear(true, 1, "alab-secrets");
        assert!(members.holder_names().is_empty());
    }

    #[test]
    fn raw_browse_order_does_not_cancel_candidate_rotation() {
        let defaults = candidate_urls(crate::ALAB_DEFAULT_URLS, &[], crate::ALAB_VAULT);
        let limit = MAX_CANDIDATES - defaults.len();
        assert!(limit > 0);
        let names: Vec<_> = (0..35).map(|n| format!("m{n:02}")).collect();
        let expected: std::collections::BTreeSet<_> = names
            .iter()
            .map(|name| format!("http://{name}.local:7484/v1/alab"))
            .collect();
        let from = AtomicUsize::new(0);
        let mut queried = std::collections::BTreeSet::new();
        for pass in 0..names.len().div_ceil(limit) {
            // A restarted browse can report the same live servers in an order
            // that cancels a cursor's progress through first-heard names.
            let mut arrivals = names.clone();
            arrivals.rotate_right((pass * limit) % names.len());
            let browse = arrivals
                .iter()
                .map(|name| added(crate::server::SERVICE_TYPE, 4, name));
            let heard = heard_instances(browse, crate::server::SERVICE_TYPE);
            assert_eq!(heard, arrivals, "the fold preserves event order");
            let discovered: Vec<_> = alab_candidate_names(heard, &from)
                .into_iter()
                .map(|name| DiscoveredVault {
                    url: format!("http://{name}.local:7484/v1/alab"),
                    txt: None,
                })
                .collect();
            let urls = candidate_urls(crate::ALAB_DEFAULT_URLS, &discovered, crate::ALAB_VAULT);
            assert!(urls.len() <= MAX_CANDIDATES);
            assert!(urls.starts_with(&defaults));
            queried.extend(urls.into_iter().filter(|url| expected.contains(url)));
        }
        assert_eq!(queried, expected);
    }

    #[test]
    fn restarted_browse_order_does_not_cancel_watch_rotation() {
        type Select = fn(&Watch) -> Vec<String>;
        let names: Vec<_> = (0..35)
            .map(|n| {
                if n == 0 {
                    "alab-secrets".to_owned()
                } else {
                    format!("alab-secrets ({})", n + 1)
                }
            })
            .collect();
        let expected: std::collections::BTreeSet<_> = names.iter().cloned().collect();
        let found_limit =
            MAX_CANDIDATES - candidate_urls(crate::ALAB_DEFAULT_URLS, &[], crate::ALAB_VAULT).len();
        let mut coverage = Vec::new();
        for (kind, select, limit) in [
            ("found", Watch::found_names as Select, found_limit),
            ("peers", Watch::peer_names as Select, MAX_CANDIDATES),
            ("holders", Watch::holder_names as Select, MAX_CANDIDATES),
        ] {
            let bell = Arc::new(Bell::default());
            let watch = Watch::idle(&bell, &[]);
            let mut queried = std::collections::BTreeSet::new();
            for pass in 0..names.len().div_ceil(limit) {
                let mut arrivals = names.clone();
                arrivals.rotate_right((pass * limit) % names.len());
                watch.began_browse();
                for name in arrivals {
                    watch.hear(true, 4, &name);
                }
                let selected = select(&watch);
                assert!(selected.len() <= limit);
                assert!(selected.iter().all(|name| expected.contains(name)));
                queried.extend(selected);
            }
            coverage.push((kind, queried.len()));
        }
        assert_eq!(coverage, [("found", 35), ("peers", 35), ("holders", 35)]);
    }

    #[test]
    fn default_urls_do_not_starve_discovered_servers_at_the_final_candidate_limit() {
        let defaults = candidate_urls(crate::ALAB_DEFAULT_URLS, &[], crate::ALAB_VAULT);
        assert!(!defaults.is_empty() && defaults.len() < MAX_CANDIDATES);
        for use_watch in [true, false] {
            for count in [16usize, 32, 35] {
                let bell = Arc::new(Bell::default());
                let watch = Watch::idle(&bell, &[]);
                let browse_from = AtomicUsize::new(0);
                let mut expected = std::collections::BTreeSet::new();
                for index in 0..count {
                    let name = format!("m{index:02}");
                    watch.hear(true, 1, &name);
                    expected.insert(format!("http://{name}.local:7484/v1/alab"));
                }
                let mut queried = std::collections::BTreeSet::new();
                for _ in 0..3 {
                    let names = if use_watch {
                        watch.found_names()
                    } else {
                        alab_candidate_names(watch.heard(), &browse_from)
                    };
                    let discovered: Vec<_> = names
                        .into_iter()
                        .map(|name| DiscoveredVault {
                            url: format!("http://{name}.local:7484/v1/alab"),
                            txt: None,
                        })
                        .collect();
                    let urls =
                        candidate_urls(crate::ALAB_DEFAULT_URLS, &discovered, crate::ALAB_VAULT);
                    assert!(urls.len() <= MAX_CANDIDATES);
                    assert!(urls.starts_with(&defaults));
                    queried.extend(urls.into_iter().filter(|url| expected.contains(url)));
                }
                assert_eq!(queried, expected, "servers={count}, watch={use_watch}");
            }
        }
    }
}
