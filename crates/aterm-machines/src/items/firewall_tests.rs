// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::model::{HostFacts, Mode, Outcome};
use crate::port::{Fake, Ran};

fn ctx() -> Ctx {
    Ctx {
        facts: HostFacts::default(),
        users: Vec::new(),
        console: None,
        host_name: "lab-07".into(),
        cache_dir: "/c".into(),
        disrupt: true,
    }
}

const FW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";

#[test]
fn every_answer_socketfilterfw_gave_in_the_vm_parses_in_both_states() {
    assert_eq!(
        Setting::Enabled.parse("Firewall is disabled. (State = 0)\n"),
        Read::Value(false)
    );
    assert_eq!(
        Setting::Enabled.parse("Firewall is enabled. (State = 1)\n"),
        Read::Value(true)
    );
    assert_eq!(
        Setting::Stealth.parse("Firewall stealth mode is off\n"),
        Read::Value(false)
    );
    assert_eq!(
        Setting::Stealth.parse("Firewall stealth mode is on\n"),
        Read::Value(true)
    );
    let both_on = "Automatically allow built-in signed software ENABLED.\nAutomatically allow downloaded signed software ENABLED.\n";
    let built_in_off = "Automatically allow built-in signed software DISABLED.\nAutomatically allow downloaded signed software ENABLED.\n";
    let both_off = "Automatically allow built-in signed software DISABLED.\nAutomatically allow downloaded signed software DISABLED.\n";
    assert_eq!(Setting::AllowSigned.parse(both_on), Read::Value(true));
    assert_eq!(Setting::AllowSigned.parse(built_in_off), Read::Value(false));
    assert_eq!(
        Setting::AllowSignedDownloaded.parse(built_in_off),
        Read::Value(true)
    );
    assert_eq!(
        Setting::AllowSignedDownloaded.parse(both_off),
        Read::Value(false)
    );
}

#[test]
fn an_answer_the_parser_does_not_know_is_unusable_not_off() {
    assert!(matches!(
        Setting::Stealth.parse("Firewall stealth mode is maybe"),
        Read::Unusable(_)
    ));
    assert!(matches!(Setting::Enabled.parse(""), Read::Unusable(_)));
}

#[test]
fn the_starter_baseline_turns_the_firewall_and_stealth_on_and_reverts_each_off() {
    let sys = Fake::root();
    sys.on(
        &format!("{FW} --getglobalstate"),
        Ran::ok("Firewall is disabled. (State = 0)\n"),
    )
    .on(
        &format!("{FW} --getglobalstate"),
        Ran::ok("Firewall is enabled. (State = 1)\n"),
    )
    .on(&format!("{FW} --setglobalstate on"), Ran::ok(""))
    .on(
        &format!("{FW} --getstealthmode"),
        Ran::ok("Firewall stealth mode is off\n"),
    )
    .on(
        &format!("{FW} --getstealthmode"),
        Ran::ok("Firewall stealth mode is on\n"),
    )
    .on(&format!("{FW} --setstealthmode on"), Ran::ok(""));
    let t = Table {
        enabled: Some(true),
        stealth: Some(true),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::Changed),
        "{rows:#?}"
    );
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(&*format!("{FW} --setglobalstate off"))
    );
    assert_eq!(
        rows[1].revert.as_deref(),
        Some(&*format!("{FW} --setstealthmode off"))
    );
}

#[test]
fn signed_downloads_are_set_with_setallowsignedapp() {
    let sys = Fake::root();
    let on = "Automatically allow built-in signed software ENABLED.\nAutomatically allow downloaded signed software ENABLED.\n";
    sys.on(&format!("{FW} --getallowsigned"), Ran::ok(on));
    let t = Table {
        allow_signed_downloaded: Some(false),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Plan, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Planned);
    assert_eq!(rows[0].id, "firewall.allow-signed-downloaded");
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(&*format!("{FW} --setallowsignedapp on"))
    );
}

#[test]
fn a_set_without_root_fails_with_socketfilterfws_own_words() {
    let sys = Fake::root();
    sys.on(
        &format!("{FW} --getstealthmode"),
        Ran::ok("Firewall stealth mode is off\n"),
    )
    .on(
        &format!("{FW} --setstealthmode on"),
        Ran {
            status: Some(255),
            stdout: b"Must be root to change settings.\n".to_vec(),
            stderr: Vec::new(),
        },
    );
    let t = Table {
        stealth: Some(true),
        ..Table::default()
    };
    let items = expand(&t, &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Failed);
    let why = rows[0].why.as_deref().unwrap();
    assert!(why.contains("exit 255: Must be root"), "{rows:#?}");
}

#[test]
fn a_baseline_asking_for_block_all_is_refused_whatever_it_asks() {
    for v in [true, false] {
        let t = Table {
            block_all: Some(v),
            ..Table::default()
        };
        let err = expand(&t, &ctx()).err().unwrap();
        assert!(err.contains("SSH"), "{err}");
    }
}
