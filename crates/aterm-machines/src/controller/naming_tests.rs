// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
            waits_on: None,
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
