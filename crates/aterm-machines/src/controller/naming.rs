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
mod tests {
    use super::*;
    use crate::model::{Effect, ItemReport, Mode};

    fn host(name: &str, n: u8, baseline: Option<&str>) -> Host {
        Host {
            name: name.into(),
            uuid: format!("0A1B2C3D-0000-4000-8000-0000000000{n:02}"),
            address: format!("{name}.local"),
            user: "admin".into(),
            baseline: baseline.map(Into::into),
            users: Vec::new(),
        }
    }

    const DESCRIBED: &str = "schema = 1\n[names]\ncomputer_name = \"{tag} {hardware}\"\nlocal_host_name = \"{tag}-{hardware}\"\n";

    #[test]
    fn a_long_inventory_name_is_named_by_its_tag_so_the_hardware_is_said_once() {
        let h = host("m10-MacBook-M4-Max-36GB-1TB", 10, None);
        assert_eq!(
            local_host_name(DESCRIBED, &h).as_deref(),
            Some("m10-{hardware}")
        );
        let filled = crate::baseline::fill(DESCRIBED, &vars(&h)).unwrap();
        assert!(filled.contains("\"m10 {hardware}\""), "{filled}");
        let plain = "schema = 1\n[names]\nlocal_host_name = \"{name}\"\n";
        assert_eq!(
            local_host_name(plain, &h).as_deref(),
            Some("m10-MacBook-M4-Max-36GB-1TB")
        );
        assert_eq!(local_host_name("schema = 1\n", &h), None);
        let by_serial = "schema = 1\n[names]\nlocal_host_name = \"mac-{serial}\"\n";
        assert_eq!(local_host_name(by_serial, &h), None);
    }

    #[test]
    fn two_macs_asked_for_one_local_hostname_are_refused_and_distinct_tags_are_not() {
        let sys = crate::port::Fake::root();
        let paths = Paths {
            root: "/ctl".into(),
        };
        sys.file("/ctl/baselines/default.toml", DESCRIBED.as_bytes())
            .file(
                "/ctl/baselines/shared.toml",
                b"schema = 1\n[names]\nlocal_host_name = \"lab\"\n",
            );
        let inv = Inventory {
            schema: 1,
            hosts: vec![
                host("m1", 1, None),
                host("m1-spare", 2, None),
                host("m10", 3, None),
                host("lab-a", 4, Some("shared")),
                host("lab-b", 5, Some("shared")),
            ],
            ..Inventory::default()
        };
        let wanted = wanted(&sys, &paths, &inv);
        let why = |i: usize| {
            let h = &inv.hosts[i];
            clash(&inv, &wanted, h, &wanted[&h.uuid.to_ascii_lowercase()])
        };
        let m1 = why(0).unwrap();
        assert!(
            m1.contains("m1 and m1-spare") && m1.contains("m1-<hardware>"),
            "{m1}"
        );
        assert!(why(1).is_some());
        assert_eq!(why(2), None);
        assert!(why(3).unwrap().contains("lab-a and lab-b"));
    }

    fn renamed(from: &str, to: &str) -> HostReport {
        HostReport {
            schema: 1,
            mode: Mode::Apply,
            serial: 1,
            key_id: String::new(),
            run_id: String::new(),
            started: 0,
            finished: 0,
            facts: crate::model::HostFacts::default(),
            items: vec![ItemReport {
                id: "names.local-host-name".into(),
                family: "names".into(),
                title: String::new(),
                outcome: Outcome::Changed,
                from: Some(format!("{from:?}")),
                to: Some(format!("{to:?}")),
                why: None,
                revert: None,
                effect: Effect::Now,
                detail: None,
                layer: None,
                class: None,
                explain: None,
                stood_aside: None,
                essential: false,
                elapsed_ms: None,
            }],
            refused: None,
        }
    }

    #[test]
    fn a_name_with_a_dot_or_underscore_is_compared_in_the_label_form_the_mac_sets() {
        let h = host("lab_03.b", 3, None);
        assert_eq!(
            local_host_name(DESCRIBED, &h).as_deref(),
            Some("lab-03-b-{hardware}")
        );
        let sys = crate::port::Fake::root();
        let paths = Paths {
            root: "/ctl".into(),
        };
        sys.file("/ctl/baselines/default.toml", DESCRIBED.as_bytes());
        let inv = Inventory {
            schema: 1,
            hosts: vec![h, host("lab-03-b", 4, None)],
            ..Inventory::default()
        };
        let wanted = wanted(&sys, &paths, &inv);
        let why = clash(
            &inv,
            &wanted,
            &inv.hosts[0],
            &wanted[&inv.hosts[0].uuid.to_ascii_lowercase()],
        );
        assert!(
            why.as_deref()
                .is_some_and(|w| w.contains("lab-03-b-<hardware>")),
            "{why:?}"
        );
    }

    fn vm_hello() -> Hello {
        Hello {
            serial_number: "ZPXWJY6KDJ".into(),
            hardware: ["Virtual Machine", "M5", "8GB", "90GB"]
                .map(String::from)
                .to_vec(),
            ..Hello::default()
        }
    }

    #[test]
    fn a_mac_its_baseline_renamed_is_looked_for_at_the_name_its_run_would_set() {
        let mut vm = host("lab-03", 3, None);
        assert_eq!(
            renamed_address(&vm, DESCRIBED, &vm_hello()).as_deref(),
            Some("lab-03-Virtual-Machine-M5-8GB-90GB.local")
        );
        vm.name = "lab_03".into();
        assert_eq!(
            renamed_address(&vm, DESCRIBED, &vm_hello()).as_deref(),
            Some("lab-03-Virtual-Machine-M5-8GB-90GB.local")
        );
        let by_serial = "schema = 1\n[names]\nlocal_host_name = \"mac-{serial}\"\n";
        assert_eq!(
            renamed_address(&vm, by_serial, &vm_hello()).as_deref(),
            Some("mac-ZPXWJY6KDJ.local")
        );
        vm.address = "LAB-03-virtual-machine-m5-8gb-90gb.local.".into();
        assert_eq!(renamed_address(&vm, DESCRIBED, &vm_hello()), None);
        vm.address = "lab-03.local".into();
        assert_eq!(renamed_address(&vm, DESCRIBED, &Hello::default()), None);
        assert_eq!(renamed_address(&vm, "schema = 1\n", &vm_hello()), None);
        vm.address = "192.168.64.4".into();
        assert_eq!(renamed_address(&vm, DESCRIBED, &vm_hello()), None);
        vm.address = "lab-03.corp.local".into();
        assert_eq!(renamed_address(&vm, DESCRIBED, &vm_hello()), None);
        let sys = crate::port::Fake::root();
        let paths = Paths {
            root: "/ctl".into(),
        };
        vm.address = "lab-03.local".into();
        sys.file("/ctl/baselines/default.toml", DESCRIBED.as_bytes());
        assert_eq!(super::renamed(&sys, &paths, &vm), None, "no hello kept yet");
        let state = super::inventory::HostState {
            hello: Some(aterm_json::to_string(&vm_hello()).unwrap()),
            ..super::inventory::HostState::default()
        };
        super::inventory::save_state(&sys, &paths, &vm.uuid, &state).unwrap();
        let found = super::renamed(&sys, &paths, &vm).unwrap();
        assert_eq!(found.address, "lab-03-Virtual-Machine-M5-8GB-90GB.local");
        assert_eq!((found.name, found.uuid), (vm.name, vm.uuid));
    }

    #[test]
    fn a_mac_reached_by_a_stale_dot_local_name_is_looked_for_at_the_one_it_says_it_has() {
        let mut vm = host("lab-03", 3, None);
        let mut said = vm_hello();
        said.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
        assert_eq!(
            own_address(&vm, &said).as_deref(),
            Some("lab-03-Virtual-Machine-M5-8GB-90GB.local")
        );
        said.local_host_name = "LAB-03".into();
        assert_eq!(own_address(&vm, &said), None);
        for odd in ["", "-lab", "lab 03", "lab.03"] {
            said.local_host_name = odd.into();
            assert_eq!(own_address(&vm, &said), None, "{odd:?}");
        }
        said.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
        vm.address = "192.168.64.4".into();
        assert_eq!(own_address(&vm, &said), None);
        said.local_host_name = "lab-03".into();
        vm.address = "lab-03.corp.local".into();
        assert_eq!(own_address(&vm, &said), None);
        vm.address = "lab_03.local".into();
        assert_eq!(own_address(&vm, &said), None);
    }

    #[test]
    fn a_mac_held_by_an_ip_address_is_looked_for_at_its_local_hostname_and_a_name_is_not() {
        let mut vm = host("lab-09", 9, None);
        let mut said = vm_hello();
        said.local_host_name = "lab-09".into();
        for ip in ["192.168.64.9", "fe80::1"] {
            vm.address = ip.into();
            assert_eq!(
                named_address(&vm, &said).as_deref(),
                Some("lab-09.local"),
                "{ip}"
            );
        }
        for name in ["lab-09.local", "lab-09.corp.example"] {
            vm.address = name.into();
            assert_eq!(named_address(&vm, &said), None, "{name}");
        }
        vm.address = "192.168.64.9".into();
        for odd in ["", "-lab", "lab 09", "lab.09"] {
            said.local_host_name = odd.into();
            assert_eq!(named_address(&vm, &said), None, "{odd:?}");
        }
    }

    #[test]
    fn a_dot_local_address_follows_the_new_local_hostname_and_an_ip_address_stays() {
        let mut h = host("m1", 1, None);
        h.address = "m1-Air-M2-24GB-1TB-2.local".into();
        let report = renamed("m1-Air-M2-24GB-1TB-2", "m1-MacBook-Air-M2-24GB-1TB");
        assert_eq!(
            followed_address(&h, &report).as_deref(),
            Some("m1-MacBook-Air-M2-24GB-1TB.local")
        );
        h.address = "M1-AIR-M2-24GB-1TB-2.local.".into();
        assert!(followed_address(&h, &report).is_some());
        h.address = "192.168.64.4".into();
        assert_eq!(followed_address(&h, &report), None);
        h.address = "m1.lab.example".into();
        assert_eq!(followed_address(&h, &report), None);
        h.address = "m1-Air-M2-24GB-1TB-2.local".into();
        let mut plan = report;
        plan.items[0].outcome = Outcome::Planned;
        assert_eq!(followed_address(&h, &plan), None);
    }
}
