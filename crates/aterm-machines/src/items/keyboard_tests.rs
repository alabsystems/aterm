// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, export, missing, user};
use crate::model::{Keyboard, Mode, Outcome};
use crate::port::{Fake, Ran};

const VM_CURRENT_HOST_GLOBAL: &[u8] =
    include_bytes!("../../fixtures/prefs/vm-27.0-currenthost-global.plist");

const SEEN_KEYBOARDTYPE: &str = "<dict>\n\t<key>keyboardtype</key>\n\t<dict>\n\t\t<key>321-9456-0</key>\n\t\t<integer>40</integer>\n\t\t<key>49299-1133-0</key>\n\t\t<integer>40</integer>\n\t\t<key>8352-9456-0</key>\n\t\t<integer>40</integer>\n\t</dict>\n</dict>";

const GLOBAL: &str = "admin$ /usr/bin/defaults -currentHost export -g -";
const TYPES: &str = "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -";

fn vm_ctx() -> Ctx {
    let mut c = ctx(&[user("admin", 501)]);
    c.facts.form = Form::Virtual;
    c.facts.keyboards = vec![
        Keyboard {
            vendor: 0x5ac,
            product: 0x1,
            name: "Virtual Keyboard".into(),
            builtin: true,
        },
        Keyboard {
            vendor: 0x5ac,
            product: 0x8105,
            name: "Virtual USB Keyboard".into(),
            builtin: false,
        },
    ];
    c
}

fn table(mode: &str, models: &[&str]) -> Table {
    Table {
        caps_lock: Some(mode.into()),
        models: models.iter().map(|m| (*m).to_owned()).collect(),
    }
}

fn vm_global() -> Ran {
    Ran {
        status: Some(0),
        stdout: VM_CURRENT_HOST_GLOBAL.to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn the_vm_gets_one_item_per_keyboard_hidutil_saw_and_one_for_the_seen_models() {
    let items = expand(&table("control", &["1133-49299", "1452-1-0"]), &vm_ctx()).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "keyboard.caps-lock.1452-1-0@admin",
            "keyboard.caps-lock.1452-33029-0@admin",
            "keyboard.caps-lock.1133-49299-0@admin",
            "keyboard.caps-lock.seen@admin",
        ]
    );
    assert_eq!(
        items[0].title(),
        "Caps Lock→Control on keyboard 1452-1-0 (built in) for admin"
    );
    assert!(items.iter().all(|i| i.effect() == Effect::Login));
}

#[test]
fn an_apple_silicon_laptop_maps_its_built_in_keyboard_as_0_0_0() {
    let mut c = ctx(&[user("alice", 501), user("bob", 502)]);
    c.facts.form = Form::Laptop;
    let items = expand(&table("control", &[]), &c).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "keyboard.caps-lock.0-0-0@alice",
            "keyboard.caps-lock.seen@alice",
            "keyboard.caps-lock.0-0-0@bob",
            "keyboard.caps-lock.seen@bob",
        ]
    );
    c.facts.arch = "x86_64".into();
    let items = expand(&table("control", &[]), &c).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn a_table_that_does_not_say_what_caps_lock_does_is_refused_or_empty() {
    assert!(expand(&Table::default(), &vm_ctx()).unwrap().is_empty());
    let err = expand(
        &Table {
            caps_lock: None,
            models: vec!["1-2".into()],
        },
        &vm_ctx(),
    )
    .err()
    .unwrap();
    assert!(err.contains("caps_lock"), "{err}");
    let err = expand(&table("ctrl", &[]), &vm_ctx()).err().unwrap();
    assert!(err.contains("\"control\" or \"caps-lock\""), "{err}");
    for bad in ["0x5ac-0x1", "1452", "1452-1-1", "a-b", "1-2-0-0"] {
        let err = expand(&table("control", &[bad]), &vm_ctx()).err().unwrap();
        assert!(err.contains("not <vendor>-<product>"), "{bad}: {err}");
    }
}

#[test]
fn the_vms_mapped_keyboard_probes_ok_and_its_unmapped_one_is_written() {
    let sys = Fake::root();
    sys.on(GLOBAL, vm_global());
    let items = expand(&table("control", &[]), &vm_ctx()).unwrap();
    let c = vm_ctx();
    assert_eq!(
        items[0].probe(&sys, &c),
        Probe::Ok {
            now: "Caps Lock→Left Control".into()
        }
    );
    assert_eq!(
        items[1].probe(&sys, &c),
        Probe::Change {
            from: "no remap".into(),
            to: "Caps Lock→Control".into()
        }
    );

    let sys = Fake::root();
    let after = String::from_utf8_lossy(VM_CURRENT_HOST_GLOBAL).replace(
        "\t<key>com.apple.trackpad.enableSecondaryClick</key>",
        "\t<key>com.apple.keyboard.modifiermapping.1452-33029-0</key>\n\t<array>\n\t\t<dict>\n\t\t\t<key>HIDKeyboardModifierMappingDst</key>\n\t\t\t<integer>30064771296</integer>\n\t\t\t<key>HIDKeyboardModifierMappingSrc</key>\n\t\t\t<integer>30064771129</integer>\n\t\t</dict>\n\t</array>\n\t<key>com.apple.trackpad.enableSecondaryClick</key>",
    );
    sys.on(GLOBAL, vm_global());
    sys.on(GLOBAL, vm_global());
    sys.on(GLOBAL, Ran::ok(&after));
    sys.on(
        "admin$ /usr/bin/defaults -currentHost write -g com.apple.keyboard.modifiermapping.1452-33029-0 '<array><dict><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer></dict></array>'",
        Ran::ok(""),
    );
    let one: Vec<Box<dyn Item>> = items.into_iter().skip(1).take(1).collect();
    let rows = engine::run(&one, Mode::Apply, &sys, &c, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(
            "admin$ /usr/bin/defaults -currentHost delete -g com.apple.keyboard.modifiermapping.1452-33029-0  # it was not set before this run"
        )
    );
}

fn entry(src: i64, dst: i64) -> PValue {
    PValue::Dict(vec![
        (DST.into(), PValue::Int(dst)),
        (SRC.into(), PValue::Int(src)),
    ])
}

fn mapping(pairs: &[(i64, i64)]) -> PValue {
    PValue::Array(pairs.iter().map(|(s, d)| entry(*s, *d)).collect())
}

const LEFT_COMMAND: i64 = 0x7_0000_00E3;
const LEFT_OPTION: i64 = 0x7_0000_00E2;

#[test]
fn caps_lock_to_the_right_control_system_settings_writes_counts_as_done() {
    let read = Read::Value(mapping(&[(CAPS_LOCK, RIGHT_CONTROL)]));
    assert_eq!(plan(&read, CapsLock::Control), Ok(None));
    assert_eq!(describe(&read), "Caps Lock→Right Control");
}

#[test]
fn another_remap_the_user_made_is_kept_when_caps_lock_is_mapped_or_unmapped() {
    let read = Read::Value(mapping(&[(LEFT_COMMAND, LEFT_OPTION)]));
    assert_eq!(
        plan(&read, CapsLock::Control),
        Ok(Some(Want::Value(PValue::Array(vec![
            entry(LEFT_COMMAND, LEFT_OPTION),
            caps_to_control(),
        ]))))
    );
    let read = Read::Value(mapping(&[
        (CAPS_LOCK, 0x7_0000_0029),
        (LEFT_COMMAND, LEFT_OPTION),
    ]));
    let Ok(Some(Want::Value(PValue::Array(next)))) = plan(&read, CapsLock::Control) else {
        panic!("no plan");
    };
    assert_eq!(next[0], caps_to_control());
    assert_eq!(int(&next[1], SRC), Some(LEFT_COMMAND));
    let read = Read::Value(mapping(&[
        (CAPS_LOCK, LEFT_CONTROL),
        (LEFT_COMMAND, LEFT_OPTION),
    ]));
    assert_eq!(
        plan(&read, CapsLock::CapsLock),
        Ok(Some(Want::Value(mapping(&[(LEFT_COMMAND, LEFT_OPTION)]))))
    );
    let read = Read::Value(mapping(&[(CAPS_LOCK, LEFT_CONTROL)]));
    assert_eq!(plan(&read, CapsLock::CapsLock), Ok(Some(Want::Unset)));
    assert_eq!(plan(&Read::Absent, CapsLock::CapsLock), Ok(None));
}

#[test]
fn caps_lock_back_on_the_vm_deletes_its_only_mapping() {
    let sys = Fake::root();
    sys.on(GLOBAL, vm_global());
    sys.on(GLOBAL, vm_global());
    sys.on(GLOBAL, missing());
    let delete = "admin$ /usr/bin/defaults -currentHost delete -g com.apple.keyboard.modifiermapping.1452-1-0";
    sys.on(delete, Ran::ok(""));
    let c = vm_ctx();
    let items = expand(&table("caps-lock", &[]), &c).unwrap();
    let one: Vec<Box<dyn Item>> = items.into_iter().take(1).collect();
    let rows = engine::run(&one, Mode::Apply, &sys, &c, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(rows[0].from.as_deref(), Some("Caps Lock→Left Control"));
    assert_eq!(rows[0].to, Some(describe(&Read::Absent)));
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.as_str() == delete)
            .count(),
        1
    );
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(
            "admin$ /usr/bin/defaults -currentHost write -g com.apple.keyboard.modifiermapping.1452-1-0 '<array><dict><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer></dict></array>'  # restores the value before this run"
        )
    );
}

#[test]
fn a_key_holding_something_other_than_mappings_is_left_alone_as_unusable() {
    let read = Read::Value(PValue::Str("x".into()));
    let err = plan(&read, CapsLock::Control).unwrap_err();
    assert!(err.contains("left alone"), "{err}");
}

#[test]
fn keyboardtype_names_a_model_product_first() {
    let entries = crate::plist::parse_domain(export(SEEN_KEYBOARDTYPE).out().as_bytes()).unwrap();
    assert_eq!(
        seen_models(&entries).unwrap(),
        [
            Model {
                vendor: 1133,
                product: 49299
            },
            Model {
                vendor: 9456,
                product: 321
            },
            Model {
                vendor: 9456,
                product: 8352
            },
        ]
    );
    assert!(seen_models(&Vec::new()).unwrap().is_empty());
}

#[test]
fn a_keyboardtype_domain_it_cannot_read_is_unusable_never_no_keyboards() {
    let c = vm_ctx();
    let items = expand(&table("control", &[]), &c).unwrap();
    let seen = items.iter().find(|i| i.id().contains("seen")).unwrap();
    for (body, needle) in [
        (
            "<dict>\n\t<key>keyboardtype</key>\n\t<string>x</string>\n</dict>",
            "not a dictionary of keyboard models",
        ),
        (
            "<dict>\n\t<key>keyboardtype</key>\n\t<dict>\n\t\t<key>Logitech</key>\n\t\t<integer>40</integer>\n\t</dict>\n</dict>",
            "is not <product>-<vendor>-<n>",
        ),
    ] {
        let sys = Fake::root();
        sys.on(TYPES, export(body));
        match seen.probe(&sys, &c) {
            Probe::Unusable { why } => assert!(why.contains(needle), "{why}"),
            other => panic!("{needle}: {other:?}"),
        }
    }
}

#[test]
fn the_seen_models_item_maps_each_seen_model_the_others_do_not_cover() {
    let c = vm_ctx();
    let items = expand(&table("control", &["9456-321"]), &c).unwrap();
    let seen: Vec<Box<dyn Item>> = items
        .into_iter()
        .filter(|i| i.id().contains("seen"))
        .collect();
    let sys = Fake::root();
    sys.on(TYPES, export(SEEN_KEYBOARDTYPE));
    sys.on(GLOBAL, vm_global());
    sys.on(GLOBAL, vm_global());
    let mapped = String::from_utf8_lossy(VM_CURRENT_HOST_GLOBAL).replace(
        "\t<key>com.apple.trackpad.enableSecondaryClick</key>",
        "\t<key>com.apple.keyboard.modifiermapping.1133-49299-0</key>\n\t<array>\n\t\t<dict>\n\t\t\t<key>HIDKeyboardModifierMappingDst</key>\n\t\t\t<integer>30064771296</integer>\n\t\t\t<key>HIDKeyboardModifierMappingSrc</key>\n\t\t\t<integer>30064771129</integer>\n\t\t</dict>\n\t</array>\n\t<key>com.apple.keyboard.modifiermapping.9456-8352-0</key>\n\t<array>\n\t\t<dict>\n\t\t\t<key>HIDKeyboardModifierMappingDst</key>\n\t\t\t<integer>30064771296</integer>\n\t\t\t<key>HIDKeyboardModifierMappingSrc</key>\n\t\t\t<integer>30064771129</integer>\n\t\t</dict>\n\t</array>\n\t<key>com.apple.trackpad.enableSecondaryClick</key>",
    );
    sys.on(GLOBAL, Ran::ok(&mapped));
    for model in ["1133-49299-0", "9456-8352-0"] {
        sys.on(
            &format!("admin$ /usr/bin/defaults -currentHost write -g com.apple.keyboard.modifiermapping.{model} '<array><dict><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer></dict></array>'"),
            Ran::ok(""),
        );
    }
    let rows = engine::run(&seen, Mode::Apply, &sys, &c, &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(
        rows[0].from.as_deref(),
        Some("1133-49299-0: no remap; 9456-8352-0: no remap")
    );
    let revert = rows[0].revert.clone().unwrap();
    assert!(
        revert.contains("delete -g com.apple.keyboard.modifiermapping.1133-49299-0"),
        "{revert}"
    );
    assert_eq!(revert.lines().count(), 2, "{revert}");
    assert!(!sys.calls().iter().any(|c| c.contains("9456-321")));
}

#[test]
fn a_mac_that_has_seen_no_other_keyboard_probes_ok() {
    let sys = Fake::root();
    sys.on(TYPES, missing());
    let c = vm_ctx();
    let items = expand(&table("control", &[]), &c).unwrap();
    let seen = items.iter().find(|i| i.id().contains("seen")).unwrap();
    assert_eq!(
        seen.probe(&sys, &c),
        Probe::Ok {
            now: "no other keyboard model seen".into()
        }
    );
    assert_eq!(sys.calls(), [TYPES]);
}

/// A change only to keyboard models the read-back did not list is to new targets: macOS
/// listed them since, and nobody set their Caps Lock. A change to a model it listed is not,
/// alone or beside a new one, nor is one that names no model.
#[test]
fn a_change_only_to_models_the_read_back_did_not_list_is_to_new_targets() {
    let seen = SeenKeyboards {
        user: user("admin", 501),
        mode: CapsLock::Control,
        covered: Vec::new(),
        before: Mutex::default(),
    };
    let held = "1133-49299-0: CapsLock→LeftControl";
    assert!(seen.new_targets(held, "9456-8352-0: no remap"));
    assert!(seen.new_targets("no other keyboard model seen", "9456-8352-0: no remap"));
    assert!(!seen.new_targets(held, "1133-49299-0: no remap"));
    assert!(!seen.new_targets(held, "1133-49299-0: no remap; 9456-8352-0: no remap"));
    assert!(!seen.new_targets(held, "no other keyboard model seen"));
    assert!(!seen.new_targets(held, "not a model: no remap"));
}
