// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn document() -> crate::text::Document {
    let mut document = crate::text::Document::new_vault(
        "m0",
        &crate::age::Identity::generate()
            .unwrap()
            .recipient()
            .to_bech32(),
    );
    for n in 1..7 {
        document
            .add_machine(
                &format!("m{n}"),
                &crate::age::Identity::generate()
                    .unwrap()
                    .recipient()
                    .to_bech32(),
            )
            .unwrap();
    }
    document
}

#[test]
fn control_text_requires_one_explicit_signed_audience() {
    let named = format!("[m1 m2]\n{ENTRY}=inert-control\n");
    assert_eq!(
        crate::text::parse_locker(named.as_bytes()).unwrap().len(),
        1
    );
    assert!(crate::text::parse_plain(named.as_bytes()).is_err());
    for text in [
        format!("[everyone]\n{ENTRY}=inert-control\n"),
        format!("[m1]\n{ENTRY}<<END if-absent\ninert-control\nEND\n"),
        format!("[m1]\n{ENTRY}=one\n[m2]\n{ENTRY}=two\n"),
        format!("[m1]\n{}=inert-control\n", ENTRY.to_uppercase()),
        format!("[m1]\n~/.secrets/{ENTRY}=inert-control\n"),
        "[m1]\naterm-commons-control-v2=inert-future-control\n".into(),
    ] {
        assert!(crate::text::parse_locker(text.as_bytes()).is_err());
    }
}

#[test]
fn control_audience_changes_are_atomic_and_do_not_accumulate_empty_lockers() {
    use crate::text::Audience;
    let mut document = document();
    for bits in 1u16..=80 {
        let hosts = (0..7)
            .filter(|n| bits & (1 << n) != 0)
            .map(|n| format!("m{n}"))
            .collect();
        document
            .set_entry(&Audience::Hosts(hosts), ENTRY, b"inert-control")
            .unwrap();
        assert_eq!(
            document
                .entries()
                .iter()
                .filter(|entry| entry.name == ENTRY)
                .count(),
            1
        );
        assert_eq!(
            document.lockers().len(),
            2,
            "membership changes do not consume the 62-audience limit"
        );
    }
    let before = document.render();
    for audience in [Audience::Everyone, Audience::Hosts(vec!["unknown".into()])] {
        assert!(
            document
                .set_entry(&audience, ENTRY, b"invalid replacement")
                .is_err()
        );
        assert_eq!(
            document.render(),
            before,
            "the existing descriptor survives a failed move"
        );
    }
    document
        .set_entry(&Audience::Hosts(vec!["m1".into()]), ENTRY, b"inert-control")
        .unwrap();
    let before = document.render();
    assert!(document.remove_machine("m1", true).is_err());
    assert_eq!(
        document.render(),
        before,
        "forced member cleanup cannot erase the network descriptor"
    );
    document
        .set_entry(&Audience::Hosts(vec!["m2".into()]), ENTRY, b"inert-control")
        .unwrap();
    assert!(document.remove_machine("m1", true).unwrap());
}

#[test]
fn moving_control_preserves_other_entries_in_its_previous_audience() {
    use crate::text::Audience;
    let mut document = document();
    let previous = Audience::Hosts(vec!["m1".into()]);
    document
        .set_entry(&previous, "ordinary", b"keep this")
        .unwrap();
    document
        .set_entry(&previous, ENTRY, b"inert-control")
        .unwrap();
    document
        .set_entry(&Audience::Hosts(vec!["m2".into()]), ENTRY, b"inert-control")
        .unwrap();
    assert_eq!(
        document
            .entry(&previous, "ordinary")
            .unwrap()
            .value
            .as_slice(),
        b"keep this"
    );
    assert!(document.entry(&previous, ENTRY).is_none());
    assert_eq!(document.lockers().len(), 3);
}
