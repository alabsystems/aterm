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
#[path = "discover_mesh_hint_tests.rs"]
mod mesh_hint_tests;

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
#[path = "discover_tests.rs"]
mod tests;
