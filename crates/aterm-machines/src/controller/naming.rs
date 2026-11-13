// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Baseline naming, Local hostname collision checks, and `.local` address tracking.

use super::inventory::{self, Host, Inventory, Paths};
use crate::hardware::{self, Description, PLACEHOLDER};
use crate::items::names::{self, tag};
use crate::model::{HostReport, Outcome};
use crate::port::System;
use crate::protocol::Hello;
use std::collections::BTreeMap;

/// The placeholders the controller fills for `host` before the baseline is signed.
#[must_use]
pub fn vars(host: &Host) -> [(&'static str, &str); 3] {
    [
        ("name", host.name.as_str()),
        ("tag", tag(&host.name)),
        ("uuid", host.uuid.as_str()),
    ]
}

fn template(text: &str, host: &Host, serial: Option<&str>) -> Option<String> {
    let mut vars = vars(host).to_vec();
    if let Some(serial) = serial {
        vars.push(("serial", serial));
    }
    let filled = crate::baseline::fill(text, &vars).ok()?;
    let doc: aterm_toml::Value = aterm_toml::from_str(&filled).ok()?;
    Some(
        doc.get("names")?
            .get("local_host_name")?
            .as_str()?
            .to_owned(),
    )
}

/// The Local hostname `text` asks for on `host`, in label form with `{hardware}` unfilled.
#[must_use]
pub fn local_host_name(text: &str, host: &Host) -> Option<String> {
    let name = template(text, host, None)?;
    (!name.contains("{serial}")).then(|| hardware::label_form_keeping(&name, PLACEHOLDER))
}

fn baseline_text(sys: &dyn System, paths: &Paths, name: &str) -> Option<String> {
    if name.contains('/') || name.contains("..") {
        return None;
    }
    let bytes = sys.read_file(&paths.baseline(name)).ok()?;
    String::from_utf8(bytes).ok()
}

/// The Local hostname each host's baseline asks for, keyed by uuid.
#[must_use]
pub fn wanted(sys: &dyn System, paths: &Paths, inv: &Inventory) -> BTreeMap<String, String> {
    let mut texts: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for host in &inv.hosts {
        let name = host.baseline.as_deref().unwrap_or("default").to_owned();
        let text = texts
            .entry(name)
            .or_insert_with_key(|name| baseline_text(sys, paths, name));
        if let Some(local) = text.as_deref().and_then(|t| local_host_name(t, host)) {
            out.insert(host.uuid.to_ascii_lowercase(), local);
        }
    }
    out
}

fn dot_local(host: &Host) -> Option<&str> {
    let address = host.address.trim_end_matches('.');
    let (name, domain) = address.split_once('.')?;
    (domain.eq_ignore_ascii_case("local") && names::dns_label(name)).then_some(name)
}

/// The `.local` address `host` answers at once its baseline (`text`) renamed it.
#[must_use]
pub fn renamed_address(host: &Host, text: &str, hello: &Hello) -> Option<String> {
    let now = dot_local(host)?;
    let template = template(text, host, Some(&hello.serial_number))?;
    let hardware = Description::from_parts(&hello.hardware);
    let local = names::planned_local_host_name(&template, hardware.as_ref())?;
    (!local.eq_ignore_ascii_case(now)).then(|| format!("{local}.local"))
}

/// The `.local` address of the Local hostname `hello` reports when it differs from `host`.
#[must_use]
pub fn own_address(host: &Host, hello: &Hello) -> Option<String> {
    let now = dot_local(host)?;
    let own = hello.local_host_name.as_str();
    (names::dns_label(own) && !own.eq_ignore_ascii_case(now)).then(|| format!("{own}.local"))
}

/// The `.local` address of `hello`'s Local hostname for a host held by IP address.
#[must_use]
pub fn named_address(host: &Host, hello: &Hello) -> Option<String> {
    host.address.parse::<std::net::IpAddr>().ok()?;
    let own = hello.local_host_name.as_str();
    names::dns_label(own).then(|| format!("{own}.local"))
}

/// Return `host` at its Local hostname from the newest hello in `state/`.
#[must_use]
pub fn named(sys: &dyn System, paths: &Paths, host: &Host) -> Option<Host> {
    let state = inventory::load_state(sys, paths, &host.uuid).ok()?;
    let hello: Hello = aterm_json::from_str(state.hello.as_deref()?).ok()?;
    let address = named_address(host, &hello)?;
    Some(Host {
        address,
        ..host.clone()
    })
}

/// Return `host` at the address its baseline renamed it to using the newest hello in `state/`.
#[must_use]
pub fn renamed(sys: &dyn System, paths: &Paths, host: &Host) -> Option<Host> {
    let state = inventory::load_state(sys, paths, &host.uuid).ok()?;
    let hello: Hello = aterm_json::from_str(state.hello.as_deref()?).ok()?;
    let text = baseline_text(sys, paths, host.baseline.as_deref().unwrap_or("default"))?;
    let address = renamed_address(host, &text, &hello)?;
    Some(Host {
        address,
        ..host.clone()
    })
}

/// Explain why `host` cannot be given the Local hostname `local` when another host claims it.
#[must_use]
pub fn clash(
    inv: &Inventory,
    wanted: &BTreeMap<String, String>,
    host: &Host,
    local: &str,
) -> Option<String> {
    let other = inv.hosts.iter().find(|o| {
        !o.uuid.eq_ignore_ascii_case(&host.uuid)
            && wanted
                .get(&o.uuid.to_ascii_lowercase())
                .is_some_and(|w| w.eq_ignore_ascii_case(local))
    })?;
    Some(format!(
        "{} and {} would both get the Local hostname {}: give one of them another name in \
         machines.toml ([names] fills {{tag}} from the name's leading letters and digits)",
        host.name,
        other.name,
        local.replace(PLACEHOLDER, "<hardware>")
    ))
}

/// The address `host` answers at after `report` changed its Local hostname.
#[must_use]
pub fn followed_address(host: &Host, report: &HostReport) -> Option<String> {
    let row = report
        .items
        .iter()
        .find(|r| r.id == "names.local-host-name" && r.outcome == Outcome::Changed)?;
    let unquote = |s: &Option<String>| {
        let s = s.as_deref()?.strip_prefix('"')?.strip_suffix('"')?;
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            .then(|| s.to_owned())
    };
    let (from, to) = (unquote(&row.from)?, unquote(&row.to)?);
    dot_local(host)?
        .eq_ignore_ascii_case(&from)
        .then(|| format!("{to}.local"))
}

#[cfg(test)]
#[path = "naming_tests.rs"]
mod tests;
