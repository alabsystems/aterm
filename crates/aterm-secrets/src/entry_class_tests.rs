// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const TOKEN: &str = "aterm-claude-token-ac-0123456789abcdef";

#[test]
fn each_name_a_consumer_imports_is_classed_as_its_role_needs() {
    let release = EntryClass::Held(Held::Release);
    for name in [RELEASE_GH_TOKEN, NOTARY, SITE_DEPLOYER_KEY]
        .into_iter()
        .chain(RELEASE_RING)
        .chain(LEGACY_RELEASE_RING)
        .chain(PUBLIC_ROSTER)
        .chain(LEGACY_ROSTER)
        .chain([
            "~/.aterm/apple/devid-m22.cer",
            "~/.aterm/apple/devid-m22.key",
        ])
    {
        assert_eq!(of(name), release, "{name}");
    }
    let fleet = EntryClass::Held(Held::FleetConfiguration);
    for name in FLEET_CONFIGURATION
        .map(str::to_owned)
        .into_iter()
        .chain(["codex", "grok", "kimi"].map(cli_accounts))
        .chain([format!("{NATIVE_API_KEY_PREFIX}{}", "0a".repeat(24))])
        // A configuration file no consumer has named yet is held too: never a credential
        // leaf a member rotates.
        .chain(
            [
                "future-policy.toml",
                "lab-routes.json",
                "sheet.csv",
                "x.yaml",
            ]
            .map(String::from),
        )
    {
        assert_eq!(of(&name), fleet, "{name}");
        assert!(member_refusal(&name).is_some(), "{name}");
    }
    // CONTROLS: an allocation's name is exactly 48 hex digits; a credential leaf is shared.
    for name in [
        format!("{NATIVE_API_KEY_PREFIX}{}", "0a".repeat(23)),
        format!("{NATIVE_API_KEY_PREFIX}{}", "zz".repeat(24)),
    ] {
        assert_eq!(of(&name), EntryClass::Shared, "{name}");
    }
    assert_eq!(of(GIT_CONFIG), EntryClass::Held(Held::GitConfig));
    for name in [
        "gh_access_token",
        GH_HOSTS,
        GIT_CREDENTIALS,
        "mailbox-field-x",
    ] {
        assert_eq!(of(name), EntryClass::Shared, "{name}");
    }
    let control = EntryClass::Consumed(Consumed::Control);
    for name in [
        FLEET_CODE,
        crate::SWARM_ROSTER_ENTRY,
        crate::SWARM_POLICY_ENTRY,
        crate::SWARM_LEASE_ENTRY,
        crate::commons::ENTRY,
        "aterm-commons-control-v2",
    ] {
        assert_eq!(of(name), control, "{name}");
        assert!(reserved(name), "{name}");
    }
    for name in [TOKEN.to_owned(), format!("{TOKEN}{CLAUDE_TOKEN_META}")] {
        assert_eq!(of(&name), EntryClass::Consumed(Consumed::ClaudeToken));
        assert!(reserved(&name) && claude_token(&name), "{name}");
    }
    for consumed in [
        EntryClass::Consumed(Consumed::Control),
        EntryClass::Consumed(Consumed::ClaudeToken),
    ] {
        assert!(!consumed.materialized());
    }
    assert!(release.materialized() && fleet.materialized() && EntryClass::Shared.materialized());
}

#[test]
fn case_variants_and_other_home_paths_are_set_apart() {
    for name in [
        "GH_ACCESS_TOKEN_ALABSYSTEMS",
        "Release-Notary",
        "ATERM-CLAUDE-TOKEN-AC-0123456789ABCDEF",
        "aterm-claude-token-ac-0123456789ABCDEF",
        "Fleet-Accounts.toml",
        "AI-Accounts-codex.toml",
        "ATERM-FLEET-CODE",
        "~/.Config/git/config",
        "~/.ATERM/release-ring/machine.key",
        "~/.aterm/apple/devid-M22.cer",
    ] {
        assert_eq!(
            of(name),
            EntryClass::SetApart(SetApart::CaseVariant),
            "{name}"
        );
        let words = set_apart_refusal(name).expect("refused");
        assert!(words.contains(&name.to_ascii_lowercase()), "{words}");
        assert!(
            reserved(name) == !of(&name.to_ascii_lowercase()).materialized(),
            "{name}"
        );
    }
    for name in [
        "~/.zshrc",
        "~/Library/LaunchAgents/x.plist",
        "~/.ssh/config",
        "~/.config/git/hooks/x",
        "~/.Config/gh/hosts.yml",
        "~/.codex/config.toml",
    ] {
        assert_eq!(of(name), EntryClass::SetApart(SetApart::HomePath), "{name}");
    }
    assert_eq!(
        of("~/.codex/auth.json"),
        EntryClass::SetApart(SetApart::CliLogin)
    );
    for name in [
        "aterm-claude-token-",
        "aterm-claude-token-ac-0123",
        "aterm-claude-token-ac-0123456789abcdef-other",
        "Aterm-Claude-Token-x",
    ] {
        assert_eq!(
            of(name),
            EntryClass::SetApart(SetApart::TokenNamespace),
            "{name}"
        );
    }
    assert!(text_refusal(TOKEN).is_some() && set_apart_refusal(TOKEN).is_none());
    assert!(text_refusal("gh_access_token").is_none());
    assert!(
        text_refusal(RELEASE_GH_TOKEN).is_none(),
        "the admin route keeps Held names"
    );
}

/// A PLAIN NAME HELD ONLY BY ITS CONFIGURATION SUFFIX IS HELD IN EVERY SPELLING: no consumer
/// reads it by name, so `Notes.json`, a credential a person set before the suffix default,
/// stays a file every Mac writes and an admin sets, refused to a member alone; two spellings of
/// it still make a folded pair. CONTROL: a mixed-case spelling of fleet configuration this
/// module names stays a case variant, set apart.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with
/// cmp): without `of`'s suffix-held arm, `Notes.json` reads `SetApart(CaseVariant)`.
#[test]
fn a_suffix_held_name_is_held_in_every_spelling() {
    let fleet = EntryClass::Held(Held::FleetConfiguration);
    for name in ["Notes.json", "notes.json", "App-Info.plist", "Config.TOML"] {
        assert_eq!(of(name), fleet, "{name}");
        assert!(of(name).materialized(), "{name}");
        assert!(text_refusal(name).is_none(), "{name}: an admin sets it");
        assert!(member_refusal(name).is_some(), "{name}: a member does not");
    }
    assert_eq!(
        folded_pairs(["Notes.json", "notes.json"]),
        BTreeMap::from([("Notes.json", "notes.json"), ("notes.json", "Notes.json")])
    );
    for name in [
        "Fleet-Accounts.toml",
        "AI-Accounts-codex.toml",
        "Mailbox.toml",
    ] {
        assert_eq!(
            of(name),
            EntryClass::SetApart(SetApart::CaseVariant),
            "CONTROL: {name}"
        );
    }
    let native = format!(
        "{}{}",
        NATIVE_API_KEY_PREFIX.to_ascii_uppercase(),
        "0a".repeat(24)
    );
    assert_eq!(
        of(&native),
        EntryClass::SetApart(SetApart::CaseVariant),
        "CONTROL: {native}"
    );
}

#[test]
fn a_folded_pair_names_both_and_only_names_written_as_files() {
    let pairs = folded_pairs([
        "Foo",
        "foo",
        "bar",
        "GH_ACCESS_TOKEN_ALABSYSTEMS",
        RELEASE_GH_TOKEN,
    ]);
    assert_eq!(pairs, BTreeMap::from([("Foo", "foo"), ("foo", "Foo")]));
    assert!(folded_pairs(["foo", "foo", "bar"]).is_empty());
    assert!(folded_pair_refusal("Foo", "foo").contains("unset `foo` first"));
}
