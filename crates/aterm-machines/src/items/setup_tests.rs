// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, export, missing, user};
use crate::model::{Mode, Outcome, Probe};
use crate::port::{Fake, Ran};

const VM_SKIPPED: &[u8] =
    include_bytes!("../../fixtures/prefs/vm-27.0-setupassistant-skipped.plist");

const SEEN_ON_27_0_1: &str = "<dict>\n\t<key>DidSeeAccessibility</key>\n\t<true/>\n\t<key>DidSeeAppStore</key>\n\t<false/>\n\t<key>DidSeeNewFeaturesProductVersion</key>\n\t<string>26.5</string>\n\t<key>LastSeenSiriProductVersion</key>\n\t<string>27.0.1</string>\n\t<key>LastSeenBuddyBuildVersion</key>\n\t<string>22F2083</string>\n</dict>";

const EXPORT: &str = "admin$ /usr/bin/defaults export com.apple.SetupAssistant -";

#[test]
fn every_key_is_one_item_per_user_at_this_hosts_versions() {
    let users = [user("admin", 501), user("student", 502)];
    let items = expand(
        &Table {
            skip_assistant: Some(true),
        },
        &ctx(&users),
    )
    .unwrap();
    assert_eq!(items.len(), KEYS.len() * 2);
    assert_eq!(items[0].id(), "setup.DidSeeAccessibility@admin");
    assert_eq!(items[KEYS.len()].id(), "setup.DidSeeAccessibility@student");
    let titles: Vec<String> = items.iter().map(|i| i.title()).collect();
    assert!(
        titles.contains(
            &"Setup Assistant LastSeenSiriProductVersion = \"27.0\" for admin".to_owned()
        )
    );
    assert!(titles.contains(
        &"Setup Assistant LastSeenBuddyBuildVersion = \"26A428\" for student".to_owned()
    ));
    assert!(
        items
            .iter()
            .all(|i| i.family() == "setup" && i.effect() == Effect::Login)
    );
}

#[test]
fn skip_assistant_false_or_absent_leaves_setup_assistant_alone() {
    let c = ctx(&[user("admin", 501)]);
    assert!(expand(&Table::default(), &c).unwrap().is_empty());
    assert!(
        expand(
            &Table {
                skip_assistant: Some(false)
            },
            &c
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn the_vms_empty_domain_plans_every_key_and_its_written_domain_probes_ok() {
    let c = ctx(&[user("admin", 501)]);
    let items = expand(
        &Table {
            skip_assistant: Some(true),
        },
        &c,
    )
    .unwrap();
    let sys = Fake::root();
    sys.on(EXPORT, missing());
    let rows = engine::run(&items, Mode::Plan, &sys, &c, &mut NoJournal);
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::Planned),
        "{rows:?}"
    );
    let sys = Fake::root();
    sys.on(
        EXPORT,
        Ran {
            status: Some(0),
            stdout: VM_SKIPPED.to_vec(),
            stderr: Vec::new(),
        },
    );
    let rows = engine::run(&items, Mode::Apply, &sys, &c, &mut NoJournal);
    assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
    assert!(!sys.calls().iter().any(|c| c.contains(" write ")));
}

#[test]
fn a_pane_seen_on_an_older_version_or_not_at_all_is_planned_again() {
    let mut c = ctx(&[user("admin", 501)]);
    c.facts.os_version = "27.0.1".into();
    c.facts.os_build = "26A434".into();
    let items = expand(
        &Table {
            skip_assistant: Some(true),
        },
        &c,
    )
    .unwrap();
    let sys = Fake::root();
    sys.on(EXPORT, export(SEEN_ON_27_0_1));
    let probe = |key: &str| {
        items
            .iter()
            .find(|i| i.id() == format!("setup.{key}@admin"))
            .unwrap()
            .probe(&sys, &c)
    };
    assert_eq!(
        probe("DidSeeAccessibility"),
        Probe::Ok { now: "true".into() }
    );
    assert_eq!(
        probe("DidSeeAppStore"),
        Probe::Change {
            from: "false".into(),
            to: "true".into()
        }
    );
    assert_eq!(
        probe("LastSeenSiriProductVersion"),
        Probe::Ok {
            now: "\"27.0.1\"".into()
        }
    );
    assert_eq!(
        probe("DidSeeNewFeaturesProductVersion"),
        Probe::Change {
            from: "\"26.5\"".into(),
            to: "\"27.0.1\"".into()
        }
    );
    assert_eq!(
        probe("LastSeenBuddyBuildVersion"),
        Probe::Change {
            from: "\"22F2083\"".into(),
            to: "\"26A434\"".into()
        }
    );
}

#[test]
fn a_version_key_waits_when_the_hosts_version_was_not_read() {
    let mut c = ctx(&[user("admin", 501)]);
    c.facts.os_version.clear();
    let items = expand(
        &Table {
            skip_assistant: Some(true),
        },
        &c,
    )
    .unwrap();
    let siri = items
        .iter()
        .find(|i| i.id() == "setup.LastSeenSiriProductVersion@admin")
        .unwrap();
    let sys = Fake::root();
    assert!(
        matches!(siri.probe(&sys, &c), Probe::Blocked { why } if why.contains("macOS version"))
    );
    let flag = items
        .iter()
        .find(|i| i.id() == "setup.DidSeeSiriSetup@admin")
        .unwrap();
    sys.on(EXPORT, missing());
    assert!(matches!(flag.probe(&sys, &c), Probe::Change { .. }));
}

#[test]
fn the_key_list_matches_what_the_vm_read_back() {
    let crate::model::PValue::Dict(entries) = crate::plist::parse(VM_SKIPPED).unwrap() else {
        panic!("not a dict");
    };
    let mut read: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    let mut keys: Vec<&str> = KEYS.iter().map(|(k, _, _)| *k).collect();
    read.sort_unstable();
    keys.sort_unstable();
    assert_eq!(read, keys);
}
