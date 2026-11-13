// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A substring search admits the comment and nested-dictionary negative controls.
#[test]
fn only_unique_root_dictionary_declarations_are_capabilities() {
    let declarations = "<key>ATermHandoffGrantChunks</key><string>chunks1</string>\
        <key>ATermHandoffGrantEmpty</key><string>empty1</string>\
        <key>ATermHandoffGrantDesk</key><string>desk1</string>\
        <key>ATermNativeRecipientRecovery</key><string>bound-recipients5</string>";
    let wrap = |body: &str| format!("<plist version=\"1.0\"><dict>{body}</dict></plist>");
    let all = Capabilities {
        grant_chunks: true,
        grant_empty: true,
        grant_desk: true,
        recipient_recovery: NativeRecipientRecovery::BoundJournal,
    };
    assert_eq!(Capabilities::from_plist(&wrap(declarations)), all);
    for text in [
        wrap(&format!("<!-- {declarations} -->")),
        wrap(&format!("<key>nested</key><dict>{declarations}</dict>")),
        wrap(&format!(
            "<key>nested</key><array><dict>{declarations}</dict></array>"
        )),
        wrap(&format!("{declarations}{declarations}")),
        format!("<plist><dict>{declarations}</dict>"),
        format!("{}<plist><dict/></plist>", wrap(declarations)),
        declarations.to_string(),
    ] {
        assert_eq!(
            Capabilities::from_plist(&text),
            Capabilities::default(),
            "{text}"
        );
    }
    let recovery = |value: &str| wrap(&format!("<key>ATermNativeRecipientRecovery</key>{value}"));
    for value in [
        "<string>recipients6</string>",
        "<string>Recipients5</string>",
        "<string>bound-recipients6</string>",
        "<string>Bound-recipients5</string>",
        "<true/>",
        "<string/>",
        "<string><string>recipients5</string></string>",
        "<string><string>bound-recipients5</string></string>",
    ] {
        assert_eq!(
            Capabilities::from_plist(&recovery(value)),
            Capabilities::default()
        );
    }
    assert_eq!(
        Capabilities::from_plist(&recovery("<string> recipients&#53; </string>"))
            .recipient_recovery,
        NativeRecipientRecovery::LegacyJournal
    );
    assert_eq!(
        Capabilities::from_plist(&recovery("<string> bound-recipients&#53; </string>"))
            .recipient_recovery,
        NativeRecipientRecovery::BoundJournal
    );
}

#[test]
fn legacy_reader_evidence_never_becomes_bound_journal_support() {
    for (token, expected) in [
        (
            NATIVE_LEGACY_RECIPIENT_RECOVERY_TOKEN,
            NativeRecipientRecovery::LegacyJournal,
        ),
        (
            NATIVE_RECIPIENT_RECOVERY_TOKEN,
            NativeRecipientRecovery::BoundJournal,
        ),
        ("unknown", NativeRecipientRecovery::Absent),
    ] {
        let declaration =
            format!("<key>ATermNativeRecipientRecovery</key><string>{token}</string>");
        let text = format!("<plist><dict>{declaration}</dict></plist>");
        assert_eq!(super::super::plist_recipient_recovery(&text), expected);
        for body in [
            format!(
                "{declaration}<key>ATermNativeRecipientRecovery</key><string>bound-recipients5</string>"
            ),
            format!("<key>nested</key><dict>{declaration}</dict>"),
            format!("<!-- {declaration} -->"),
        ] {
            assert_eq!(
                super::super::plist_recipient_recovery(&format!(
                    "<plist><dict>{body}</dict></plist>"
                )),
                NativeRecipientRecovery::Absent
            );
        }
    }
}
