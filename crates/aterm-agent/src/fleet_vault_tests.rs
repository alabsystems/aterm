// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn vault(name: &str, keys: &[u8]) -> Vault {
    Vault {
        name: name.to_owned(),
        keys: keys.iter().map(|byte| [*byte; 32]).collect(),
    }
}

/// A record reads back as the vault it records, and nothing else reads as one.
#[test]
fn a_record_reads_back_as_its_vault_and_nothing_else_does() {
    let alab = vault("alab", &[1, 2]);
    assert_eq!(Vault::of_text(&alab.text()), Some(alab.clone()));
    let named = vault("a vault, named \"so\"", &[3]);
    assert_eq!(Vault::of_text(&named.text()), Some(named));
    let text = alab.text();
    for broken in [
        String::new(),
        HEAD.to_owned(),
        text.replace("name ", "nom "),
        text.lines()
            .filter(|line| !line.starts_with("key"))
            .collect::<Vec<_>>()
            .join("\n"),
        text.lines()
            .filter(|line| !line.starts_with("name"))
            .collect::<Vec<_>>()
            .join("\n"),
        format!("{text}name 61\n"),
        format!("{text}key {}\n", "01".repeat(32)),
        format!("{text}key {}\n", "AB".repeat(32)),
        format!("{text}key 01\n"),
        "name \nkey 0101010101010101010101010101010101010101010101010101010101010101\n".to_owned(),
    ] {
        assert_eq!(Vault::of_text(&broken), None, "{broken:?}");
    }
}

/// The same vault through a rotation, and none other: the name kept and a key of the line
/// kept. A record widened by the line configured now stays that vault through a chain of
/// rotations whose line alone keeps no key of the first.
#[test]
fn a_vault_is_itself_through_its_rotations_and_no_other() {
    let first = vault("alab", &[1]);
    assert!(first.is(&vault("alab", &[2, 1])), "a rotation");
    assert!(
        !first.is(&vault("alab", &[3])),
        "another line under the name"
    );
    assert!(!first.is(&vault("other", &[1])), "another name");
    let widened = first.widened(&vault("alab", &[2, 1])).expect("a key added");
    assert_eq!(widened, vault("alab", &[1, 2]));
    assert_eq!(
        widened.widened(&vault("alab", &[2])),
        None,
        "nothing to add"
    );
    assert!(
        widened.is(&vault("alab", &[2])),
        "the line re-pinned at the rotated key alone (a join at another place)"
    );
    assert!(
        !first.is(&vault("alab", &[2])),
        "CONTROL: the record not widened"
    );
    assert_eq!(first.widened(&vault("other", &[1])), None);
}
