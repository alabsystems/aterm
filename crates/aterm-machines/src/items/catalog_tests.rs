// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, export, missing, user};
use crate::model::{Mode, Outcome, Probe, Read};
use crate::port::{Fake, Ran, System};

fn all(value: bool) -> Table {
    Table {
        siri: Some(value),
        siri_data_sharing: Some(value),
        analytics: Some(value),
        personalized_ads: Some(value),
        airdrop: Some(value),
        widgets: Some(value),
        universal_control: None,
        bluetooth_setup_assistant: Some(value),
        ds_store_on_network: Some(value),
    }
}

#[test]
fn every_preset_off_expands_to_its_rows_per_user_and_its_system_rows_once() {
    let users = [user("alice", 501), user("bob", 502)];
    let items = expand(&all(false), &ctx(&users)).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "consumer.siri@alice",
            "consumer.siri@bob",
            "consumer.siri-data-sharing@alice",
            "consumer.siri-data-sharing@bob",
            "consumer.analytics.auto-submit",
            "consumer.analytics.consent-version",
            "consumer.analytics.third-party",
            "consumer.personalized-ads@alice",
            "consumer.personalized-ads@bob",
            "consumer.airdrop@alice",
            "consumer.airdrop@bob",
            "consumer.widgets.desktop@alice",
            "consumer.widgets.desktop@bob",
            "consumer.widgets.stage-manager@alice",
            "consumer.widgets.stage-manager@bob",
            "consumer.widgets.notification-center@alice",
            "consumer.widgets.notification-center@bob",
            "consumer.bluetooth-setup-assistant.keyboard",
            "consumer.bluetooth-setup-assistant.pointing-device",
            "consumer.ds-store.network@alice",
            "consumer.ds-store.network@bob",
            "consumer.ds-store.usb@alice",
            "consumer.ds-store.usb@bob",
        ]
    );
    assert!(items.iter().all(|i| i.family() == "consumer"));
    assert_eq!(
        items[0].title(),
        "Siri off for alice (com.apple.assistant.support Assistant Enabled)"
    );
    assert_eq!(items[9].effect(), Effect::Relaunch);
    assert_eq!(items[17].effect(), Effect::Restart);
}

#[test]
fn the_notification_center_row_clears_the_list_only_with_the_grant() {
    use crate::items::defaults::tests::{Session, session_of};
    let alice = user("alice", 501);
    let off = Table {
        widgets: Some(false),
        ..Table::default()
    };
    let mut granted = ctx(std::slice::from_ref(&alice));
    granted.facts.full_disk_access = Some(true);
    let items = expand(&off, &granted).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "consumer.widgets.desktop@alice",
            "consumer.widgets.stage-manager@alice",
            "consumer.widgets.notification-center@alice"
        ]
    );
    let center: Vec<Box<dyn Item>> = items.into_iter().skip(2).collect();
    let (sys, session) = session_of(Session::running(&["NotificationCenter"]));
    let list = PValue::Dict(vec![
        ("vers".into(), PValue::Int(1)),
        ("future".into(), PValue::Bool(true)),
        (
            "instances".into(),
            PValue::Array(vec![PValue::Data(vec![42])]),
        ),
    ]);
    sys.run(&Target::user("com.apple.notificationcenterui", "widgets", &alice).write_cmd(&list))
        .unwrap();
    session.lock().unwrap().events.clear();
    let rows = engine::run(&center, Mode::Apply, &sys, &granted, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(rows[0].from.as_deref(), Some("1 widget"));
    assert_eq!(
        session.lock().unwrap().events,
        [
            "write com.apple.notificationcenterui widgets",
            "kill NotificationCenter"
        ]
    );
    let again = engine::run(&center, Mode::Apply, &sys, &granted, &mut NoJournal);
    assert_eq!(again[0].outcome, Outcome::Ok, "{again:?}");
    assert_eq!(session.lock().unwrap().kills("NotificationCenter"), 1);
    let Read::Value(cleared) =
        Target::user("com.apple.notificationcenterui", "widgets", &alice).read(&sys)
    else {
        panic!("the list reads back");
    };
    assert_eq!(
        cleared,
        PValue::Dict(vec![
            ("vers".into(), PValue::Int(1)),
            ("future".into(), PValue::Bool(true)),
            ("instances".into(), PValue::Array(Vec::new())),
        ])
    );

    for unknown in [None, Some(false)] {
        let mut context = ctx(std::slice::from_ref(&alice));
        context.facts.full_disk_access = unknown;
        let center: Vec<Box<dyn Item>> = expand(&off, &context)
            .unwrap()
            .into_iter()
            .skip(2)
            .collect();
        let sys = Fake::root();
        let rows = engine::run(&center, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(
            rows[0].why.as_deref().unwrap().contains("Full Disk Access"),
            "{rows:?}"
        );
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }

    let on = Table {
        widgets: Some(true),
        ..Table::default()
    };
    let ids: Vec<String> = expand(&on, &granted)
        .unwrap()
        .iter()
        .map(|i| i.id())
        .collect();
    assert_eq!(
        ids,
        [
            "consumer.widgets.desktop@alice",
            "consumer.widgets.stage-manager@alice"
        ]
    );
}

#[test]
fn a_table_that_names_nothing_expands_to_nothing() {
    let items = expand(&Table::default(), &ctx(&[user("alice", 501)])).unwrap();
    assert!(items.is_empty());
}

#[test]
fn universal_control_is_refused_because_atpkg_owns_it() {
    let table = Table {
        universal_control: Some(false),
        ..Table::default()
    };
    let err = expand(&table, &ctx(&[])).err().unwrap();
    assert!(err.contains("atpkg"), "{err}");
    assert!(
        CATALOG
            .iter()
            .all(|r| !r.domain.contains("universalcontrol"))
    );
}

#[test]
fn off_writes_the_off_value_and_on_writes_the_on_value_or_deletes() {
    let alice = user("alice", 501);
    let sys = Fake::root();
    sys.on(
        "alice$ /usr/bin/defaults export com.apple.sharingd -",
        export("<dict>\n\t<key>DiscoverableMode</key>\n\t<string>Everyone</string>\n</dict>"),
    );
    sys.on(
        "alice$ /usr/bin/defaults export com.apple.assistant.support -",
        missing(),
    );
    let off = expand(
        &Table {
            airdrop: Some(false),
            siri: Some(false),
            ..Table::default()
        },
        &ctx(std::slice::from_ref(&alice)),
    )
    .unwrap();
    let rows = engine::run(
        &off,
        Mode::Plan,
        &sys,
        &ctx(std::slice::from_ref(&alice)),
        &mut NoJournal,
    );
    assert_eq!(rows[0].id, "consumer.siri@alice");
    assert_eq!(rows[0].to.as_deref(), Some("false"));
    assert_eq!(rows[1].id, "consumer.airdrop@alice");
    assert_eq!(rows[1].from.as_deref(), Some("\"Everyone\""));
    assert_eq!(rows[1].to.as_deref(), Some("\"Off\""));
    assert!(rows.iter().all(|r| r.outcome == Outcome::Planned));

    let on = expand(
        &Table {
            airdrop: Some(true),
            siri: Some(true),
            ..Table::default()
        },
        &ctx(std::slice::from_ref(&alice)),
    )
    .unwrap();
    let rows = engine::run(
        &on,
        Mode::Plan,
        &sys,
        &ctx(std::slice::from_ref(&alice)),
        &mut NoJournal,
    );
    assert_eq!(rows[0].to.as_deref(), Some("true"));
    assert_eq!(rows[1].to.as_deref(), Some("absent"));
    assert!(rows[1].revert.as_deref().unwrap().starts_with(
        "alice$ /usr/bin/defaults write com.apple.sharingd DiscoverableMode -string Everyone"
    ));
}

#[test]
fn personalized_ads_already_off_in_the_vm_probes_ok() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.AdLib -",
        export("<dict>\n\t<key>CKDPIDSyncState</key>\n\t<integer>1</integer>\n\t<key>CKSubscribedAccountsScrubbed</key>\n\t<true/>\n\t<key>allowApplePersonalizedAdvertising</key>\n\t<false/>\n\t<key>allowIdentifierForAdvertising</key>\n\t<true/>\n\t<key>personalizedAdsMigrated</key>\n\t<true/>\n</dict>"),
    );
    let items = expand(
        &Table {
            personalized_ads: Some(false),
            ..Table::default()
        },
        &ctx(std::slice::from_ref(&admin)),
    )
    .unwrap();
    assert_eq!(
        items[0].probe(&sys, &ctx(&[admin])),
        Probe::Ok {
            now: "false".into()
        }
    );
}

#[test]
fn the_bluetooth_assistant_row_turns_the_true_macos_stores_into_false() {
    let sys = Fake::root();
    let key = "/usr/bin/defaults export /Library/Preferences/com.apple.Bluetooth -";
    sys.on(
        key,
        export("<dict>\n\t<key>BluetoothAutoSeekKeyboard</key>\n\t<true/>\n\t<key>BluetoothAutoSeekPointingDevice</key>\n\t<true/>\n</dict>"),
    );
    sys.on(
        key,
        export("<dict>\n\t<key>BluetoothAutoSeekKeyboard</key>\n\t<false/>\n\t<key>BluetoothAutoSeekPointingDevice</key>\n\t<true/>\n</dict>"),
    );
    sys.on(
        "/usr/bin/defaults write /Library/Preferences/com.apple.Bluetooth BluetoothAutoSeekKeyboard -bool false",
        Ran::ok(""),
    );
    sys.file(
        "/Library/Preferences/com.apple.Bluetooth.plist",
        b"bplist00",
    );
    let items = expand(
        &Table {
            bluetooth_setup_assistant: Some(false),
            ..Table::default()
        },
        &ctx(&[]),
    )
    .unwrap();
    let first: Vec<Box<dyn Item>> = items.into_iter().take(1).collect();
    let rows = engine::run(&first, Mode::Apply, &sys, &ctx(&[]), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(rows[0].from.as_deref(), Some("true"));
    assert!(
        sys.calls()
            .contains(&"own 0:0 644 /Library/Preferences/com.apple.Bluetooth.plist".to_owned()),
        "{:?}",
        sys.calls()
    );
}
