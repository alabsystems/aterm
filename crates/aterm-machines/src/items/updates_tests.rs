// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, export};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran};

/// Exported SoftwareUpdate preferences fixture from a clean macOS 27 VM.
const VM_SOFTWARE_UPDATE: &[u8] =
    include_bytes!("../../fixtures/prefs/vm-27.0-softwareupdate.plist");

/// Switches as a configured macOS 27 Mac holds them after System Settings.
const SWITCHED_ON_27_0_1: &str = "<dict>\n\t<key>AutomaticDownload</key>\n\t<true/>\n\t<key>AutomaticallyInstallMacOSUpdates</key>\n\t<true/>\n\t<key>ConfigDataInstall</key>\n\t<true/>\n\t<key>CriticalUpdateInstall</key>\n\t<true/>\n</dict>";

const EXPORT: &str = "/usr/bin/defaults export /Library/Preferences/com.apple.SoftwareUpdate -";
const PLIST: &str = "/Library/Preferences/com.apple.SoftwareUpdate.plist";

const SCHEDULE: &str = "/usr/sbin/softwareupdate --schedule";

/// `softwareupdate --schedule`, VM.
const SCHEDULE_ON: &str = "Automatic checking for updates is turned on\n";

#[test]
fn checking_reads_from_schedule_and_off_is_blocked_by_what_27_does() {
    let sys = Fake::root();
    sys.on(SCHEDULE, Ran::ok(SCHEDULE_ON));
    let on = expand(
        &Table {
            automatic_check: Some(true),
            ..Table::default()
        },
        &ctx(&[]),
    )
    .unwrap();
    let rows = engine::run(&on, Mode::Apply, &sys, &ctx(&[]), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Ok);
    let off = expand(
        &Table {
            automatic_check: Some(false),
            ..Table::default()
        },
        &ctx(&[]),
    )
    .unwrap();
    let rows = engine::run(&off, Mode::Apply, &sys, &ctx(&[]), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Blocked);
    let mut older = ctx(&[]);
    older.facts.os_version = "26.6.2".into();
    let rows = engine::run(&off, Mode::Apply, &sys, &older, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Blocked);
    assert!(
        rows[0].why.as_deref().unwrap().contains("26.6.2"),
        "{rows:?}"
    );
    assert!(
        !sys.calls().iter().any(|c| c.ends_with("--schedule off")),
        "{:?}",
        sys.calls()
    );
    let garbled = Fake::root();
    garbled.on(SCHEDULE, Ran::ok("something else\n"));
    let rows = engine::run(&on, Mode::Plan, &garbled, &ctx(&[]), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Unknown);
}

fn lab() -> Table {
    Table {
        automatic_check: Some(true),
        automatic_download: Some(true),
        automatic_macos_install: Some(false),
        critical: Some(true),
    }
}

#[test]
fn critical_covers_both_security_keys_and_each_key_is_a_system_item() {
    let items = expand(&lab(), &ctx(&[])).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "updates.automatic-check",
            "updates.automatic-download",
            "updates.automatic-macos-install",
            "updates.critical",
            "updates.config-data",
        ]
    );
    assert!(
        items
            .iter()
            .all(|i| i.family() == "updates" && i.effect() == Effect::Now)
    );
    assert_eq!(
        items[2].title(),
        "Software Update: install macOS updates off (com.apple.SoftwareUpdate AutomaticallyInstallMacOSUpdates)"
    );
    assert!(expand(&Table::default(), &ctx(&[])).unwrap().is_empty());
}

#[test]
fn the_vms_unset_switches_are_written_as_root_and_read_back() {
    let sys = Fake::root();
    sys.on(
        EXPORT,
        Ran {
            status: Some(0),
            stdout: VM_SOFTWARE_UPDATE.to_vec(),
            stderr: Vec::new(),
        },
    );
    sys.on(EXPORT, export(SWITCHED_ON_27_0_1));
    sys.file(PLIST, b"bplist00");
    sys.on(
        "/usr/bin/defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload -bool true",
        Ran::ok(""),
    );
    let items: Vec<Box<dyn Item>> = expand(
        &Table {
            automatic_download: Some(true),
            ..Table::default()
        },
        &ctx(&[]),
    )
    .unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(&[]), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(rows[0].from.as_deref(), Some("absent"));
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(
            "/usr/bin/defaults delete /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload  # it was not set before this run"
        )
    );
    assert!(
        sys.calls().iter().all(|c| !c.contains('$')),
        "{:?}",
        sys.calls()
    );
}

#[test]
fn switches_system_settings_already_set_probe_ok_and_a_differing_one_is_planned() {
    let sys = Fake::root();
    sys.on(EXPORT, export(SWITCHED_ON_27_0_1));
    sys.on(SCHEDULE, Ran::ok(SCHEDULE_ON));
    let items = expand(&lab(), &ctx(&[])).unwrap();
    let rows = engine::run(&items, Mode::Plan, &sys, &ctx(&[]), &mut NoJournal);
    let outcomes: Vec<Outcome> = rows.iter().map(|r| r.outcome).collect();
    assert_eq!(
        outcomes,
        [
            Outcome::Ok, // checking: --schedule reads on
            Outcome::Ok,
            Outcome::Planned, // automatic macOS installs: true, wanted false
            Outcome::Ok,
            Outcome::Ok,
        ]
    );
    assert_eq!(rows[2].from.as_deref(), Some("true"));
    assert_eq!(rows[2].to.as_deref(), Some("false"));
}
