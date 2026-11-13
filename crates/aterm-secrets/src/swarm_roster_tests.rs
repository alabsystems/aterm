// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn reserved_entry_errors_do_not_quote_values() {
    let private = "private-vault-value-must-not-escape";
    let malformed = format!("value = [\"{private}\" !]\n");
    let syntax = [
        SwarmRoster::from_toml(&malformed).unwrap_err(),
        SwarmPolicy::from_toml(&malformed).unwrap_err(),
        SwarmLease::from_toml(&malformed).unwrap_err(),
    ];
    for error in syntax {
        assert!(!error.to_string().contains(private));
        assert!(error.to_string().contains("line 1"));
    }
    let member = "[[member]]\nrecipient = 'recipient'\nvault_name = 'vault'\n";
    let semantics = [
        SwarmRoster::from_toml(&format!("{member}label = '{private}'\n")).unwrap_err(),
        SwarmRoster::from_toml(&format!(
            "{member}label = 'm1'\nclaimed_label = '{private}'\n"
        ))
        .unwrap_err(),
        SwarmPolicy::from_toml(&format!("[[steward]]\nlabel = '{private}'\n")).unwrap_err(),
    ];
    for error in semantics {
        assert!(!error.to_string().contains(private));
    }
}

/// Negative control: dropping historical aliases when rendering loses the revoked key.
#[test]
fn historical_label_denials_survive_without_an_allocator() {
    let old = r#"deny_labels = ["m4"]
deny_nodes = []
[[member]]
recipient = "historical-recipient"
vault_name = "old-name"
label = "m100"
aliases = ["m4"]
node_key = "historical-node-key"
"#;
    let roster = SwarmRoster::from_toml(old).unwrap();
    assert!(roster.is_member_denied(&roster.members[0]));
    let copy = SwarmRoster::from_toml(&roster.to_toml()).unwrap();
    assert_eq!(roster, copy);
    assert!(copy.is_member_denied(&copy.members[0]));
}

#[test]
fn local_hostname_tags_are_leading_only() {
    assert_eq!(leading_m_label("M22.local"), Some((22, "m22".into())));
    assert_eq!(leading_m_label("m0"), Some((0, "m0".into())));
    for word in ["Mac-mini-M4", "macbook-m3", "m3pro"] {
        assert_eq!(leading_m_label(word), None);
    }
}
