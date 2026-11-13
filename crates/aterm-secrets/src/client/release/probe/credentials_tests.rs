// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::parse_notary_profile_text;

/// NEGATIVE CONTROL: returning on the first `notary_profile` match or skipping lines
/// without `=` would accept duplicate top-level keys or multiline-injected assignments.
#[test]
fn notary_profile_parser_accepts_toml_strings_and_rejects_duplicates_or_multiline_values() {
    assert_eq!(
        parse_notary_profile_text(
            "# comment\nsigning_key = \"x\"\nnotary_profile = 'aterm-notary' # inline\n[extra]\nnotary_profile = \"ignored\"\n"
        )
        .unwrap(),
        "aterm-notary"
    );
    for bad in [
        "signing_key = \"x\"\n",
        "notary_profile = \"first\"\nnotary_profile = \"second\"\n",
        "\"notary_profile\" = \"first\"\nnotary_profile = \"second\"\n",
        "note = \"\"\"\nnotary_profile = \"injected\"\n\"\"\"\n",
        "signing_key = \"x\"\nbare_word\nnotary_profile = \"aterm\"\n",
        "notary_profile = \"-flag\"\n",
        "notary_profile = \"\"\n",
    ] {
        assert!(parse_notary_profile_text(bad).is_err(), "{bad}");
    }
}
