// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `_alab-vault._tcp` mDNS browse and candidate URL ordering for the ALab vault, and the standing
//! browse ([`Watch`]) of that service or of another: the node job's `_alab._tcp`, whose adverts
//! are a fleet's dial hints ([`Watch::adverts`]).
//!
//! Discovered TXT records (`seq`, `signer`, `head`) are ordering hints only: callers verify every
//! fetched envelope against the pinned signer and choose the highest **verified** `seq`. An
//! `_alab._tcp` advert is a hint the same way: whoever dials it still proves who answers.

use crate::transport::{self, Source};
use crate::vault_txt::VaultTxt;
#[cfg(unix)]
use crate::wake::{Bell, Wake};
#[cfg(target_os = "macos")]
use aterm_uds::lan::{self, LanError};
#[cfg(any(all(test, unix), target_os = "macos"))]
use aterm_uds::lan::{Event, Instance};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// Cap on candidate URLs probed in one pass.
pub const MAX_CANDIDATES: usize = 16;

/// Cap on member service instances resolved by one joiner's browse. Repeated calls in one
/// process rotate through a stable larger set without raising the per-attempt limit.
pub const MAX_MEMBERS: usize = 64;

/// The domain every browse and resolve here names: mDNS's own.
#[cfg(target_os = "macos")]
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
#[cfg(any(all(test, unix), target_os = "macos"))]
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
#[cfg(any(all(test, unix), target_os = "macos"))]
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

/// The members browse's lab knob: set, [`browse_members`] browses the lab service it names and
/// no other ([`member_service`]).
pub use crate::making::LAB_MEMBER_SERVICE_ENV;

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
             or digits>._tcp`), so this machine browses for no member"
        ))),
    }
}

/// The vault look's lab knob: set, every vault browse the look runs ([`browse_alab`],
/// [`browse_names`], the subscriber's standing [`Watch::start_look`]) browses the lab service it
/// names ([`vault_service`]), and the look asks no compiled-in [`crate::ALAB_DEFAULT_URLS`]
/// ([`look_defaults`]). A vault server's own browse ([`Watch::start`]) and advert keep
/// `_alab-vault._tcp` whatever it says.
pub use crate::making::LAB_VAULT_SERVICE_ENV;

/// The type a vault browse browses, given [`LAB_VAULT_SERVICE_ENV`]'s value (`None`: unset):
/// [`crate::vault_txt::SERVICE_TYPE`], or the [`lab_service`] the variable names.
///
/// # Errors
/// The variable is set to anything but a lab service (`_alab-vault._tcp` itself, an empty value
/// and one that is no UTF-8 included): the browse then looks for no vault, never for the real one.
pub fn vault_service(value: Option<&std::ffi::OsStr>) -> crate::Result<String> {
    let Some(value) = value else {
        return Ok(crate::vault_txt::SERVICE_TYPE.to_owned());
    };
    match value.to_str().filter(|v| lab_service(v)) {
        Some(lab) => Ok(lab.to_owned()),
        None => Err(crate::Error::new(format!(
            "{LAB_VAULT_SERVICE_ENV} names no lab service (`_alab-lab-<1 to 6 lowercase letters \
             or digits>._tcp`), so this machine browses for no vault"
        ))),
    }
}

/// [`vault_service`] of this process's [`LAB_VAULT_SERVICE_ENV`].
#[cfg(unix)]
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
#[cfg(any(all(test, unix), target_os = "macos"))]
fn vault_heard_line(service: &str, instance: &str) -> String {
    format!("vault look: heard {instance} on {service}")
}

/// The most distinct vault instances one process logs hearing ([`note_vault_heard`]); mDNS is
/// anyone's to write on the LAN, so what it logs is bounded.
#[cfg(any(all(test, unix), target_os = "macos"))]
const MOST_LOGGED: usize = 64;

/// Whether `instance` of `service` is new to `logged` (then added to it, while it holds fewer
/// than [`MOST_LOGGED`]): each instance is logged once per process, not at every look.
#[cfg(any(all(test, unix), target_os = "macos"))]
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
#[cfg(any(all(test, unix), target_os = "macos"))]
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
#[cfg(unix)]
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
#[cfg(any(all(test, unix), target_os = "macos"))]
fn reached(event: &Event) -> Option<(String, u16)> {
    let Event::Resolved { host, port, .. } = event else {
        return None;
    };
    let host = host.trim_end_matches('.');
    (*port != 0 && plain_host(host)).then(|| (host.to_ascii_lowercase(), *port))
}

/// A resolve's TXT strings that are UTF-8: one that is not names no key aterm reads.
#[cfg(any(all(test, unix), target_os = "macos"))]
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
#[cfg(any(all(test, unix), target_os = "macos"))]
fn member_target(event: &Event) -> Option<String> {
    reached(event).map(|(host, port)| format!("{host}:{port}"))
}

/// The vault `vault` a `_alab-vault._tcp` resolve answers: its URL, and the [`VaultTxt`] hint its
/// TXT carries. `None` for no usable target, or a TXT naming another path than `/v1` or another
/// vault (each key case-insensitive, its first string winning, as [`VaultTxt::from_txt`] reads).
#[cfg(any(all(test, unix), target_os = "macos"))]
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
#[cfg(any(all(test, unix), target_os = "macos"))]
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
pub const NO_BROWSE: &str = "this machine cannot browse the network for ALab's members (that needs \
     macOS's Bonjour): name one with `aterm members join --at <host>`";

/// In unit tests: nothing is browsed, and no member is found.
#[cfg(test)]
pub fn browse_members(_budget: Duration) -> crate::Result<Vec<String>> {
    Ok(Vec::new())
}

/// Off macOS no browse runs, so a joining machine names a member with `--at` ([`NO_BROWSE`]); an
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

#[cfg(any(all(test, unix), target_os = "macos"))]
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
/// `read` takes it. Where this thread works for a subscriber's pass, each name's thread adds its
/// CPU time to the pass's counts as it ends ([`crate::pass_counts::Started::run`]); where it runs
/// a server's round, to that server's ([`crate::serve_counts::Resolving::run`]).
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
    let started = &crate::pass_counts::starting();
    let serving = &crate::serve_counts::resolving();
    std::thread::scope(|scope| {
        let handles: Vec<_> = instances
            .iter()
            .map(|name| {
                scope.spawn(move || {
                    started.run(|| {
                        serving.run(|| {
                            resolve_first(name, service, resolve_budget)
                                .and_then(|event| read(&event))
                        })
                    })
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

/// Resolve each of `instances` within `budget` without collapsing equal-URL instances: two
/// holders after a heal both resolve `alab-secrets.local:7484`, distinguished by `label=` in
/// their TXT (`DESIGN-fluid-swarm` §4.1 F7), and a vault server's round gates its pulls on each
/// instance's TXT, so one instance must not replace another's TXT ([`crate::vault_txt::VaultTxt`]:
/// each instance counts in what its endpoints are pulled for). An exact repeat is kept once.
#[cfg(all(target_os = "macos", not(test)))]
fn resolve_instances(instances: &[String], budget: Duration) -> Vec<DiscoveredVault> {
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

#[cfg(all(unix, any(test, not(target_os = "macos"))))]
fn resolve_instances(_instances: &[String], _budget: Duration) -> Vec<DiscoveredVault> {
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
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
fn resolve_adverts(_service: &str, _instances: &[String], _budget: Duration) -> Vec<Advert> {
    Vec::new()
}

/// How long a browse's first events are its baseline on a watch of a service other than the
/// vault's (the node job's `_alab._tcp`): what it hears then was there before this process
/// looked, so it is held but rings nothing.
#[cfg(unix)]
const SETTLE: Duration = Duration::from_secs(30);

/// The most instances a watch of a service other than the vault's holds: one more is neither
/// held nor rung for (mDNS is anyone's to write on the LAN).
#[cfg(unix)]
const MOST_HELD: usize = 256;

/// How long every member but this process's own must go unheard, on a watch of a service other
/// than the vault's, for the next one heard to be a move: this Mac back on a LAN is one, a
/// peer's comings and goings are none. Each process names its advert afresh
/// (`aterm_agent::node_service::random_alab_instance_name`), so a peer's window or node job
/// starting again is a member never heard, and among some 40 Macs one comes or goes every half
/// minute: counted a member at a time, the browse ran the window's converge pass 1,138 times in
/// two days, at its move source's rest floor, where route changes ran it 52 times (0.155.0,
/// 2026-11-03 to 05).
#[cfg(unix)]
pub const SILENCE: Duration = Duration::from_secs(2 * 60);

/// A watch of `service`'s settle window and the most instances it holds. The vault's watch rings
/// from a browse's first event and holds every instance it hears, as it always has; a watch
/// of another service takes [`SETTLE`] and [`MOST_HELD`].
#[cfg(unix)]
fn holding(service: &str) -> (Duration, usize) {
    if service == crate::vault_txt::SERVICE_TYPE {
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
/// a browse hears within the watch's settle window. Dropping it ends the browse: its thread
/// makes each of its waits beside the read end of a pipe whose write end the watch holds (a
/// broker exchange under way finishes first). While its browse runs, and while no broker's
/// socket is there or the broker there refused this process or speaks another protocol, none of
/// them has a timer; the rest keep the backoffs [`browse_standing`] names.
#[cfg(unix)]
pub struct Watch {
    watched: Arc<Watched>,
    /// The write end of the standing browse's pipe, where one runs ([`standing`]).
    closing: Option<std::io::PipeWriter>,
}

#[cfg(unix)]
impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("service", &self.watched.service)
            .field("heard", &self.heard())
            .finish_non_exhaustive()
    }
}

/// A test's resolver for [`Watch::adverts`] ([`Watch::idle_resolving`]).
#[cfg(all(unix, any(test, debug_assertions)))]
type Resolver = Box<dyn Fn(&str) -> Option<Advert> + Send + Sync>;

/// What a test's watch resolves its vault servers' names through ([`Watch::idle_resolving_vaults`]).
#[cfg(all(test, unix))]
type VaultResolver = Box<dyn Fn(&[String], Duration) -> Vec<DiscoveredVault> + Send + Sync>;

#[cfg(unix)]
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
    /// Each wait its standing browse made ([`browse_standing`]), in order, for a test to read.
    #[cfg(all(test, target_os = "macos"))]
    waits: Mutex<Vec<Waited>>,
    /// Told at each of [`Watched::waits`].
    #[cfg(all(test, target_os = "macos"))]
    waited: std::sync::Condvar,
    /// The first restart backoff its standing browse waits ([`RESTART_FIRST`]; a test's own).
    #[cfg(all(test, target_os = "macos"))]
    restart_first: Duration,
    /// What [`Watch::adverts`] resolves through in place of a service resolve, a test's.
    #[cfg(any(test, debug_assertions))]
    resolve: Option<Resolver>,
    /// What [`Watch::peer_vaults`] and [`Watch::alab_holders`] resolve through in place of the
    /// LAN broker, a test's.
    #[cfg(test)]
    resolve_vaults: Option<VaultResolver>,
}

#[cfg(unix)]
impl Watch {
    /// Start a standing `_alab-vault._tcp` browse that rings `bell` ([`Watch::start_for`]): a
    /// vault server's, which [`LAB_VAULT_SERVICE_ENV`] does not move.
    #[must_use]
    pub fn start(bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch::start_for(crate::vault_txt::SERVICE_TYPE, bell, own)
    }

    /// Start the client look's standing vault browse that rings `bell`: [`Watch::start`], whose
    /// browse browses the type [`vault_type`] names (the lab vault service
    /// [`LAB_VAULT_SERVICE_ENV`] names in a lab's process), and none at all where that variable
    /// names no lab service.
    #[must_use]
    pub fn start_look(bell: &Arc<Bell>, own: &[&str]) -> Watch {
        let mut watched =
            Watched::new(crate::vault_txt::SERVICE_TYPE, vault_type().ok(), bell, own);
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
    /// build ([`standing`]); elsewhere, idle.
    fn running(watched: Watched) -> Watch {
        let watched = Arc::new(watched);
        #[cfg(all(target_os = "macos", not(test)))]
        let closing = standing(&watched, |browse| browse());
        #[cfg(not(all(target_os = "macos", not(test))))]
        let closing = None;
        Watch { watched, closing }
    }

    /// Seam: [`Watch::start_for`] whose standing browse runs in a test build too, inside `run`,
    /// which is handed the browse to run on the watch's thread (under the test's LAN broker,
    /// `aterm_uds::lan::seam_here`), its first restart backoff `restart_first`.
    #[cfg(all(test, target_os = "macos"))]
    fn browsing(
        service: &'static str,
        bell: &Arc<Bell>,
        own: &[&str],
        restart_first: Duration,
        run: impl FnOnce(&dyn Fn()) + Send + 'static,
    ) -> Watch {
        let mut watched = Watched::new(service, Some(service.to_owned()), bell, own);
        watched.restart_first = restart_first;
        let watched = Arc::new(watched);
        let closing = standing(&watched, run);
        Watch { watched, closing }
    }

    /// An `_alab-vault._tcp` watch that runs no browse ([`Watch::idle_for`]).
    #[must_use]
    pub fn idle(bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch::idle_for(crate::vault_txt::SERVICE_TYPE, bell, own)
    }

    /// A watch of `service` that runs no browse: what it hears reaches it only through
    /// [`Watch::hear`] (or [`Watch::feed`]), and with no browse begun, none of it is a baseline.
    #[must_use]
    pub fn idle_for(service: &'static str, bell: &Arc<Bell>, own: &[&str]) -> Watch {
        Watch {
            watched: Arc::new(Watched::new(service, Some(service.to_owned()), bell, own)),
            closing: None,
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
            closing: None,
        }
    }

    /// Seam: [`Watch::idle`] whose [`Watch::peer_vaults`] and [`Watch::alab_holders`] hand the
    /// names they would resolve through the LAN broker to `resolve` instead, for a test that
    /// feeds the watch ([`Watch::hear`]) and runs a server's round over what it resolves.
    #[cfg(test)]
    pub(crate) fn idle_resolving_vaults(
        bell: &Arc<Bell>,
        own: &[&str],
        resolve: impl Fn(&[String], Duration) -> Vec<DiscoveredVault> + Send + Sync + 'static,
    ) -> Watch {
        let service = crate::vault_txt::SERVICE_TYPE;
        let mut watched = Watched::new(service, Some(service.to_owned()), bell, own);
        watched.resolve_vaults = Some(Box::new(resolve));
        Watch {
            watched: Arc::new(watched),
            closing: None,
        }
    }

    /// Seam: take it that a browse began `ago` ago, as the standing browse does at each browse it
    /// starts with `ago` zero: what is heard while the browse is younger than the watch's settle
    /// window is its baseline, held but ringing nothing.
    #[cfg(any(test, debug_assertions))]
    pub fn child_began(&self, ago: Duration) {
        self.watched.begin(ago);
    }

    /// Forget that this process advertises `name`: its `Add` rows wake the watch again, as any
    /// peer's do. A vault server holds `alab-secrets` only while it holds the name, and the
    /// holder another server's browse hears then is a peer.
    pub fn forget_own(&self, name: &str) {
        self.watched
            .own
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|own| !own.eq_ignore_ascii_case(name));
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
    /// (`DESIGN-fluid-swarm` §4.1 F6): how many names it handed its resolver, and what they
    /// resolved to, one entry for each instance however many share a URL (an exact repeat once).
    /// A watch of another service than `_alab-vault._tcp` resolves none.
    #[must_use]
    pub fn peer_vaults(&self, budget: Duration) -> (usize, Vec<DiscoveredVault>) {
        if !self.watched.is_vault() {
            return (0, Vec::new());
        }
        let names = self.peer_names();
        if names.is_empty() {
            return (0, Vec::new());
        }
        #[cfg(test)]
        if let Some(resolve) = &self.watched.resolve_vaults {
            return (names.len(), resolve(&names, budget));
        }
        (names.len(), resolve_instances(&names, budget))
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
    /// (`DESIGN-fluid-swarm` §4.1 F7): how many names it handed its resolver, and what they
    /// resolved to. Its cursor is independent of other lookups; a watch of another service than
    /// `_alab-vault._tcp` resolves none.
    #[must_use]
    pub fn alab_holders(&self, budget: Duration) -> (usize, Vec<DiscoveredVault>) {
        let names = self.holder_names();
        if names.is_empty() {
            return (0, Vec::new());
        }
        #[cfg(test)]
        if let Some(resolve) = &self.watched.resolve_vaults {
            return (names.len(), resolve(&names, budget));
        }
        (names.len(), resolve_instances(&names, budget))
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
#[cfg(unix)]
fn alab_candidate_names(names: Vec<String>, from: &AtomicUsize) -> Vec<String> {
    let defaults = candidate_urls(look_default_urls(), &[], crate::ALAB_VAULT);
    rotated_names(names, from, MAX_CANDIDATES - defaults.len())
}

/// Stabilize a name snapshot before advancing a cursor kept across browse restarts.
/// The parser and watch still retain event order for consumers that need it.
#[cfg(unix)]
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

#[cfg(unix)]
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
            #[cfg(all(test, target_os = "macos"))]
            waits: Mutex::new(Vec::new()),
            #[cfg(all(test, target_os = "macos"))]
            waited: std::sync::Condvar::new(),
            #[cfg(all(test, target_os = "macos"))]
            restart_first: RESTART_FIRST,
            #[cfg(any(test, debug_assertions))]
            resolve: None,
            #[cfg(test)]
            resolve_vaults: None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(String, Vec<u32>)>> {
        self.instances
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this is the vault's `_alab-vault._tcp` watch.
    fn is_vault(&self) -> bool {
        self.service == crate::vault_txt::SERVICE_TYPE
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
    #[cfg(any(all(test, unix), target_os = "macos"))]
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

#[cfg(unix)]
impl Drop for Watch {
    /// Close the standing browse's pipe: its thread, which makes each of its waits beside the
    /// read end ([`browse_standing`]), ends at its wait, and with it its request to the broker.
    fn drop(&mut self) {
        drop(self.closing.take());
    }
}

/// Run `watched`'s standing browse ([`browse_standing`]) on a thread of its own, inside `run`
/// (which is handed the browse to run), beside the read end of a pipe whose write end it returns
/// for the watch to hold: dropping that end ends the browse. `None`, said in the log, where the
/// pipe or the thread could not be made.
#[cfg(target_os = "macos")]
fn standing(
    watched: &Arc<Watched>,
    run: impl FnOnce(&dyn Fn()) + Send + 'static,
) -> Option<std::io::PipeWriter> {
    use std::os::fd::AsFd as _;
    let service = watched.service;
    let (closing, open) = match aterm_uds::pipe() {
        Ok(ends) => ends,
        Err(error) => {
            crate::log_line(&format!("could not start browsing {service}: {error}"));
            return None;
        }
    };
    let watched = Arc::clone(watched);
    let spawned = std::thread::Builder::new()
        .name(format!("browse {service}"))
        .spawn(move || run(&|| browse_standing(&watched, closing.as_fd())));
    match spawned {
        Ok(_) => Some(open),
        Err(error) => {
            crate::log_line(&format!("could not start browsing {service}: {error}"));
            None
        }
    }
}

/// The first pause before a standing browse that ended (or could not start, a broker there that
/// could not answer it) starts again, doubled each time it ends within [`RESTART_LONGEST`] of its
/// start, up to that.
#[cfg(any(all(test, unix), target_os = "macos"))]
const RESTART_FIRST: Duration = Duration::from_secs(5);
#[cfg(any(all(test, unix), target_os = "macos"))]
const RESTART_LONGEST: Duration = Duration::from_secs(60);

/// How long a standing browse that found no broker waits before it asks again, where it cannot
/// watch the broker's socket for one to arrive ([`lan::Arrivals`]); and the most it sleeps where
/// poll(2) itself cannot be asked ([`Standing::wait`]).
#[cfg(target_os = "macos")]
const BROKER_PAUSE: Duration = Duration::from_secs(2);

/// How a standing browse's last start went ([`restart_pause`]).
#[cfg(any(all(test, unix), target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Started {
    /// No broker answered in a way only its next generation ends ([`waits_for_broker`]):
    /// nothing started.
    NoBroker,
    /// It started (and ran this long before it ended), or a broker there could not answer it.
    Ran(Duration),
}

/// How long a standing browse waits before it starts again after `last`, given the backoff
/// `pause` it holds, and the backoff the next end gets: the broker's absence waits for a broker
/// to arrive, with no timer of its own (`None`, [`Standing::wait_for_broker`]), and leaves the
/// backoff at [`RESTART_FIRST`]; another end waits `pause` (reset to [`RESTART_FIRST`] once a
/// browse ran [`RESTART_LONGEST`]) and doubles it, up to that.
#[cfg(any(all(test, unix), target_os = "macos"))]
fn restart_pause(last: Started, pause: Duration) -> (Option<Duration>, Duration) {
    match last {
        Started::NoBroker => (None, RESTART_FIRST),
        Started::Ran(ran) => {
            let pause = if ran >= RESTART_LONGEST {
                RESTART_FIRST
            } else {
                pause
            };
            (Some(pause), pause.saturating_mul(2).min(RESTART_LONGEST))
        }
    }
}

/// Whether a browse that could not start for `error` waits for its broker's next generation
/// ([`Standing::wait_for_broker`]) rather than out the restart backoff: every broker that did not
/// answer but one that did not judge this process (its code check did not finish, or it was
/// serving its most connections) and an exchange that failed, which a live broker can pass with
/// nothing heard. A socket there that refuses connections is a broker that went, which leaves its
/// socket behind (one that starts clears it and binds its socket again), or one between its bind
/// and its listen, which [`lan::Arrivals`] asks again on its own backoff while that generation
/// stays unanswered.
#[cfg(target_os = "macos")]
fn waits_for_broker(error: &LanError) -> bool {
    !matches!(
        error,
        LanError::Unavailable(aterm_uds::lan::Unavailable::Unchecked(_)) | LanError::Io(_)
    )
}

/// What a standing browse's thread waits for ([`Standing::wait`]).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    /// The running browse's next frame, beside its connection.
    Frames,
    /// The end of a restart's backoff, beside the broker's arrivals where they are watched.
    Restart,
    /// A broker's arrival, beside the events of its socket's folder where they are watched, and
    /// a change of macOS's record of this process's Local Network allowance where it has one.
    Broker,
}

/// macOS's record of this process's Local Network allowance ([`lan::direct::record`]), heard by a
/// [`aterm_uds::file_watch::FileWatch`] that rings a doorbell at each change of it: an Allow
/// given while no broker answers (to a program in a tab, which macOS holds aterm responsible for)
/// opens this process's direct path, which a browse asked again takes. The watch's first look
/// rings too, so the browse is asked once more after the watch is armed, and an Allow given
/// between the browse that found no broker and the arming is not lost.
#[cfg(target_os = "macos")]
struct Allowance {
    rung: Arc<aterm_uds::ready::Doorbell>,
    _watch: aterm_uds::file_watch::FileWatch,
}

#[cfg(target_os = "macos")]
impl Allowance {
    /// A watch of this thread's allowance record, for the browse of `service`: `None` where this
    /// process has no direct path or its path names no record, and, said in the log, where the
    /// watch could not start.
    fn watch(service: &str) -> Option<Self> {
        let record = lan::direct::record()?;
        let watched = aterm_uds::ready::Doorbell::new().and_then(|rung| {
            let rung = Arc::new(rung);
            let ringing = Arc::clone(&rung);
            let watch = aterm_uds::file_watch::FileWatch::platform(
                &format!("allowance {service}"),
                Box::new(move |_| ringing.ring()),
            )?;
            watch.watch(0, record);
            Ok(Self {
                rung,
                _watch: watch,
            })
        });
        watched
            .inspect_err(|error| {
                crate::log_line(&format!(
                    "browsing {service}: macOS's Local Network allowance is not watched: {error}"
                ));
            })
            .ok()
    }
}

/// One wait of a standing browse's thread ([`Standing::wait`]), noted in order for a test: what
/// it waited for, and the very timeout it handed poll(2), `None` for none. Every one is beside
/// the watch's pipe.
#[cfg(all(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Waited(Why, Option<Duration>);

#[cfg(all(test, target_os = "macos"))]
impl Watched {
    /// Note one wait of the standing browse.
    fn waited(&self, waited: Waited) {
        self.waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(waited);
        self.waited.notify_all();
    }
}

/// Browse `<type> local.` (the watch's [`Watched::browsed`] type) through the LAN broker for
/// `watched` until `closing` (the read end of the watch's pipe, [`standing`]) is readable,
/// starting it again whenever it ends (mDNSResponder restarted, or the broker did) or cannot
/// start, and hand it each event. Every wait goes through [`Standing::wait`], beside `closing`. A
/// running browse waits for its next frame with no timer. One that found no broker waits for a
/// broker to arrive ([`Standing::wait_for_broker`]): with no timer where no socket is there or
/// the broker there refused this process or speaks another protocol, on the broker arrivals' own
/// backoff (1 s doubling to 30 s) where a socket there has not answered its hello, and
/// [`BROKER_PAUSE`] where the socket's folder cannot be watched. One that ended, or that a broker
/// there could not answer, waits out its backoff ([`restart_pause`], 5 s doubling to 60 s)
/// beside the broker's arrivals, so a broker whose socket is made again meanwhile is browsed at
/// once. A browse that ended heard
/// nothing more, so what it heard is forgotten, and the next one's settle window begins with it.
/// A watch with no type to browse browses nothing.
#[cfg(target_os = "macos")]
fn browse_standing(watched: &Watched, closing: std::os::fd::BorrowedFd<'_>) {
    let Some(browsed) = watched.browsed.as_deref() else {
        crate::log_line(&format!(
            "not browsing {}: {LAB_VAULT_SERVICE_ENV} names no lab service",
            watched.service
        ));
        return;
    };
    Standing {
        watched,
        closing,
        arrivals: lan::Arrivals::new(),
        allowance: None,
    }
    .run(browsed);
}

/// A standing browse's thread ([`browse_standing`]).
#[cfg(target_os = "macos")]
struct Standing<'a> {
    watched: &'a Watched,
    /// Readable, at its end, once the watch is dropped.
    closing: std::os::fd::BorrowedFd<'a>,
    /// What says a broker arrived: a new generation of its socket that answered its checked
    /// hello. Asked before each browse, so the generation a browse meets is answered, and only a
    /// generation made since ends a wait.
    arrivals: lan::Arrivals,
    /// What says macOS's Local Network allowance for this process changed, armed at the first
    /// browse that found no broker.
    allowance: Option<Allowance>,
}

#[cfg(target_os = "macos")]
impl Standing<'_> {
    /// Browse `browsed` until the watch is dropped.
    fn run(&mut self, browsed: &str) {
        #[cfg(test)]
        let mut pause = self.watched.restart_first;
        #[cfg(not(test))]
        let mut pause = RESTART_FIRST;
        loop {
            // Armed before each attempt, as aterm-gui's browse of the network's moves is: a
            // broker made again during the attempt, or the wait after it, remains owed to that
            // wait.
            let _ = self.arrivals.poll();
            let started = Instant::now();
            let last = match lan::browse(browsed, DOMAIN) {
                Ok(mut events) => {
                    self.watched.begin(Duration::ZERO);
                    let dropped = self.hear(&mut events);
                    self.watched.forget();
                    if dropped {
                        return;
                    }
                    Started::Ran(started.elapsed())
                }
                Err(error) if waits_for_broker(&error) => Started::NoBroker,
                Err(_) => Started::Ran(started.elapsed()),
            };
            let (wait, next) = restart_pause(last, pause);
            pause = next;
            let dropped = match wait {
                Some(wait) => self.restart_after(wait),
                None => self.wait_for_broker(),
            };
            if dropped {
                return;
            }
        }
    }

    /// Wait beside the watch's pipe and `others` for at most `within` (`None`: with no timeout),
    /// noting the wait and that very timeout for a test: `None` once the watch was dropped, else
    /// which of `others` are ready (none: the timeout came). Where poll(2) cannot be asked it
    /// sleeps `within`, at most [`BROKER_PAUSE`], beside nothing, and answers that the timeout
    /// came.
    fn wait(
        &self,
        why: Why,
        others: &[std::os::fd::BorrowedFd<'_>],
        within: Option<Duration>,
    ) -> Option<Vec<usize>> {
        #[cfg(test)]
        self.watched.waited(Waited(why, within));
        #[cfg(not(test))]
        let _ = why;
        let mut fds = Vec::with_capacity(1 + others.len());
        fds.push(self.closing);
        fds.extend_from_slice(others);
        let deadline = within.and_then(|within| Instant::now().checked_add(within));
        match aterm_uds::ready::any(&fds, deadline) {
            Ok(ready) if ready.contains(&0) => None,
            Ok(ready) => Some(ready.into_iter().map(|index| index - 1).collect()),
            Err(_) => {
                std::thread::sleep(within.map_or(BROKER_PAUSE, |within| within.min(BROKER_PAUSE)));
                Some(Vec::new())
            }
        }
    }

    /// Hand the watch each event `events` hear, waiting for each frame with no timeout, until the
    /// request ends or the watch is dropped: whether it was. A frame already read off the
    /// connection is taken with no wait.
    fn hear(&self, events: &mut lan::Events) -> bool {
        use std::os::fd::AsFd as _;
        loop {
            if !events.buffered() && self.wait(Why::Frames, &[events.as_fd()], None).is_none() {
                return true;
            }
            match events.next_beside(self.closing) {
                Ok(Some(event)) => self.watched.hear_event(&event, Instant::now()),
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
    }

    /// Wait out a restart's backoff `pause` beside the broker's arrivals, or until the watch is
    /// dropped: whether it was. A new generation of the broker's socket that answers its checked
    /// hello ends the wait early ([`lan::Arrivals::poll`]); an event of the folder that brings
    /// none waits on to the same end, and an ask the arrivals' own backoff calls for is made on
    /// the way.
    fn restart_after(&mut self, pause: Duration) -> bool {
        let mut until: Option<Instant> = None;
        loop {
            let watched = match self.arrivals.poll() {
                Ok(true) => return false,
                Ok(false) => true,
                Err(_) => false,
            };
            let now = Instant::now();
            let left = match until {
                Some(until) => until.saturating_duration_since(now),
                None => {
                    until = Some(now + pause);
                    pause
                }
            };
            if left.is_zero() {
                return false;
            }
            let arriving = if watched {
                self.arrivals.watch_fd()
            } else {
                None
            };
            let within = match arriving {
                Some(_) => self.arrivals.wait_for(left),
                None => left,
            };
            let others: Vec<_> = arriving.into_iter().collect();
            if self.wait(Why::Restart, &others, Some(within)).is_none() {
                return true;
            }
        }
    }

    /// Wait for a broker to answer, or for the watch to be dropped: whether it was. A new
    /// generation of the broker's socket that answers its checked hello ends the wait
    /// ([`lan::Arrivals::poll`]), heard by the events of its socket's folder, with no timer;
    /// where a hello could not be judged (a socket nothing answers yet, a broker that did not
    /// judge this process) it is asked again on the arrivals' own backoff
    /// ([`lan::Arrivals::wait_for`], 1 s doubling to 30 s), and a generation that answered, or
    /// refused this process, or speaks another protocol, is asked nothing more until the socket
    /// is made again. Where the folder cannot be watched it waits [`BROKER_PAUSE`]. A change of
    /// macOS's record of this process's Local Network allowance ([`Allowance`]) ends the wait too,
    /// so the browse is asked again, and takes this process's direct path once macOS allowed it.
    fn wait_for_broker(&mut self) -> bool {
        if self.allowance.is_none() {
            self.allowance = Allowance::watch(self.watched.service);
        }
        loop {
            let arriving = match self.arrivals.poll() {
                Ok(true) => return false,
                Ok(false) => self.arrivals.watch_fd(),
                Err(_) => None,
            };
            let within = match arriving {
                Some(_) => {
                    let retry = self.arrivals.wait_for(Duration::MAX);
                    (retry != Duration::MAX).then_some(retry)
                }
                None => Some(BROKER_PAUSE),
            };
            let allowance = self.allowance.as_ref();
            let others: Vec<_> = arriving
                .into_iter()
                .chain(allowance.map(|allowance| allowance.rung.fd()))
                .collect();
            match self.wait(Why::Broker, &others, within) {
                None => return true,
                // macOS's allowance record changed: ask the browse again.
                Some(ready)
                    if allowance.is_some_and(|allowance| {
                        ready.contains(&(others.len() - 1)) && allowance.rung.clear()
                    }) =>
                {
                    return false;
                }
                // The pause where the folder cannot be watched came: ask again.
                Some(_) if arriving.is_none() => return false,
                // The folder changed, or the next ask is due.
                Some(_) => {}
            }
        }
    }
}

// The browse, resolve and standing-watch laws read the LAN broker's events, which only Unix has.
#[cfg(all(test, unix))]
#[path = "discover_tests.rs"]
mod tests;
