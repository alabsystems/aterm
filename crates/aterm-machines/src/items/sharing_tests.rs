// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::model::{HostFacts, Mode, Outcome};
use crate::port::Fake;

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

const VM_PRINT_DISABLED: &str = "
\tdisabled services = {
\t\t\"com.apple.screensharing\" => enabled
\t\t\"com.apple.CSCSupportd\" => disabled
\t\t\"com.apple.mdmclient.daemon.runatboot\" => disabled
\t\t\"com.apple.ftpd\" => disabled
\t\t\"com.openssh.sshd\" => enabled
\t}
";

fn not_loaded() -> Ran {
    Ran::exit(
        NOT_LOADED,
        "Bad request.\nCould not find service \"com.apple.AEServer\" in domain for system",
    )
}

fn table(rae: Option<bool>) -> Table {
    Table {
        remote_apple_events: rae,
        ..Table::default()
    }
}

#[test]
fn the_override_database_rows_are_read_as_launchctl_prints_them() {
    let rows = parse_print_disabled(VM_PRINT_DISABLED);
    assert_eq!(rows.len(), 5);
    assert!(rows.contains(&("com.openssh.sshd".into(), true)));
    assert!(rows.contains(&("com.apple.ftpd".into(), false)));
}

#[test]
fn a_label_with_no_override_takes_its_plists_disabled_key() {
    let sys = Fake::root();
    sys.on(
        "/bin/launchctl print-disabled system",
        Ran::ok(VM_PRINT_DISABLED),
    )
    .on(
        "/usr/bin/plutil -extract Disabled raw /System/Library/LaunchDaemons/com.apple.eppc.plist",
        Ran::ok("true\n"),
    );
    assert_eq!(enabled(&sys, &REMOTE_APPLE_EVENTS), Read::Value(false));
    assert_eq!(enabled(&sys, &REMOTE_LOGIN), Read::Value(true));
    let sys = Fake::root();
    sys.on("/bin/launchctl print-disabled system", Ran::ok(VM_PRINT_DISABLED))
        .on(
            "/usr/bin/plutil -extract Disabled raw /System/Library/LaunchDaemons/com.apple.eppc.plist",
            Ran::exit(1, "/System/Library/LaunchDaemons/com.apple.eppc.plist: Could not extract value, error: No value at that key path or invalid key path: Disabled"),
        );
    assert_eq!(enabled(&sys, &REMOTE_APPLE_EVENTS), Read::Value(true));
}

#[test]
fn an_override_row_in_words_launchd_did_not_use_on_27_is_unusable_not_the_plist_default() {
    let odd = format!("{VM_PRINT_DISABLED}\t\t\"com.apple.AEServer\" => true\n");
    let sys = Fake::root();
    sys.on("/bin/launchctl print-disabled system", Ran::ok(&odd))
        .on(
            "/usr/bin/plutil -extract Disabled raw /System/Library/LaunchDaemons/com.apple.eppc.plist",
            Ran::ok("true\n"),
        );
    assert!(
        matches!(enabled(&sys, &REMOTE_APPLE_EVENTS), Read::Unusable(_)),
        "{:?}",
        enabled(&sys, &REMOTE_APPLE_EVENTS)
    );
    assert!(sys.calls().iter().all(|c| !c.contains("plutil")));
}

#[test]
fn remote_apple_events_off_on_a_fresh_mac_is_already_as_wanted() {
    let sys = Fake::root();
    sys.on(
        "/bin/launchctl print-disabled system",
        Ran::ok(VM_PRINT_DISABLED),
    )
    .on(
        "/usr/bin/plutil -extract Disabled raw /System/Library/LaunchDaemons/com.apple.eppc.plist",
        Ran::ok("true\n"),
    )
    .on(
        "/bin/launchctl print system/com.apple.AEServer",
        not_loaded(),
    );
    let items = expand(&table(Some(false)), &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert_eq!(rows[0].to.as_deref(), Some("off"));
    assert!(sys.calls().iter().all(|c| !c.contains("disable system")));
}

#[test]
fn turning_remote_apple_events_on_enables_then_bootstraps_and_reverts_the_other_way() {
    let enabled_now = format!("{VM_PRINT_DISABLED}\t\t\"com.apple.AEServer\" => enabled\n");
    let sys = Fake::root();
    sys.on(
        "/bin/launchctl print-disabled system",
        Ran::ok(VM_PRINT_DISABLED),
    )
    .on(
        "/bin/launchctl print-disabled system",
        Ran::ok(&enabled_now),
    )
    .on(
        "/usr/bin/plutil -extract Disabled raw /System/Library/LaunchDaemons/com.apple.eppc.plist",
        Ran::ok("true\n"),
    )
    .on(
        "/bin/launchctl print system/com.apple.AEServer",
        not_loaded(),
    )
    .on(
        "/bin/launchctl print system/com.apple.AEServer",
        not_loaded(),
    )
    .on(
        "/bin/launchctl print system/com.apple.AEServer",
        Ran::ok("system/com.apple.AEServer = {\n\tactive count = 0\n"),
    )
    .on(
        "/bin/launchctl enable system/com.apple.AEServer",
        Ran::ok(""),
    )
    .on(
        "/bin/launchctl bootstrap system /System/Library/LaunchDaemons/com.apple.eppc.plist",
        Ran::ok(""),
    );
    let items = expand(&table(Some(true)), &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
    assert_eq!(rows[0].from.as_deref(), Some("off"));
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(
            "/bin/launchctl disable system/com.apple.AEServer; /bin/launchctl bootout system/com.apple.AEServer"
        )
    );
    let calls = sys.calls();
    let enable = calls
        .iter()
        .position(|c| c.ends_with("enable system/com.apple.AEServer"));
    let boot = calls.iter().position(|c| c.contains("bootstrap system"));
    assert!(enable < boot, "{calls:#?}");
}

#[test]
fn turning_it_off_tolerates_a_bootout_of_a_job_that_is_not_loaded() {
    let enabled_now = format!("{VM_PRINT_DISABLED}\t\t\"com.apple.AEServer\" => enabled\n");
    let disabled_now = format!("{VM_PRINT_DISABLED}\t\t\"com.apple.AEServer\" => disabled\n");
    let sys = Fake::root();
    sys.on(
        "/bin/launchctl print-disabled system",
        Ran::ok(&enabled_now),
    )
    .on(
        "/bin/launchctl print-disabled system",
        Ran::ok(&disabled_now),
    )
    .on(
        "/bin/launchctl print system/com.apple.AEServer",
        not_loaded(),
    )
    .on(
        "/bin/launchctl disable system/com.apple.AEServer",
        Ran::ok(""),
    )
    .on(
        "/bin/launchctl bootout system/com.apple.AEServer",
        Ran::exit(NO_SUCH_PROCESS, "Boot-out failed: 3: No such process"),
    );
    let items = expand(&table(Some(false)), &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].from.as_deref(), Some("enabled, not loaded"));
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
}

#[test]
fn a_baseline_that_turns_remote_login_off_is_refused_with_the_reason() {
    let t = Table {
        remote_login: Some(false),
        ..Table::default()
    };
    let err = expand(&t, &ctx()).err().unwrap();
    assert!(err.contains("cut the Mac off"), "{err}");
    let t = Table {
        remote_login: Some(true),
        ..Table::default()
    };
    assert!(
        expand(&t, &ctx())
            .err()
            .unwrap()
            .contains("[person] remote_login")
    );
    let t = Table {
        screen_sharing: Some(true),
        ..Table::default()
    };
    assert!(
        expand(&t, &ctx())
            .err()
            .unwrap()
            .contains("[person] screen_sharing")
    );
}

#[test]
fn an_unreadable_override_database_is_unknown_not_off() {
    let sys = Fake::root();
    sys.on(
        "/bin/launchctl print-disabled system",
        Ran::exit(1, "Unrecognized target specifier."),
    );
    let items = expand(&table(Some(false)), &ctx()).unwrap();
    let rows = engine::run(&items, Mode::Plan, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Unknown);
}
