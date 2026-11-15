// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::claude_token::Key;

const KEY: &str = "ac-00000000000000a1";

/// `claude-token:<key>` names one account's token by its key alone: it reads back as it was
/// written, is posted, lives in the vault, arrives as the account's token entry (its meta beside
/// it) and has no hold probe, so no holder offers it. Every word that is not `ac-<16 lower-case
/// hex>` is refused, the meta record's name among them, and the token entry is still never asked
/// for by name.
#[test]
fn a_claude_token_want_names_one_accounts_token_by_its_key() {
    let word = format!("claude-token:{KEY}");
    let want = Want::parse(&word).unwrap();
    assert_eq!(want, Want::ClaudeToken(Key::parse(KEY).unwrap()));
    assert_eq!(want.key(), word);
    assert_eq!(Want::parse(&want.key()).unwrap(), want);
    assert_eq!(want.class(), Class::VaultOnly);
    assert!(want.posted());
    assert!(!want.if_absent());
    assert!(want.catalog_retirement().is_none());
    assert_eq!(
        want.entries(),
        [
            format!("aterm-claude-token-{KEY}"),
            format!("aterm-claude-token-{KEY}-meta")
        ]
    );
    assert_eq!(want.entry(), Some(format!("aterm-claude-token-{KEY}")));
    assert!(
        want.entries().iter().all(|name| crate::reserved_name(name)),
        "both names are consumed in place and never become a file"
    );
    assert!(want.hold_entry().is_none());
    for bad in [
        "claude-token:",
        "claude-token:ac-",
        "claude-token:ac-00000000000000A1",
        "claude-token:AC-00000000000000a1",
        "claude-token:ac-0000000000000a1",
        "claude-token:ac-00000000000000a10",
        "claude-token:00000000000000a1",
        "claude-token:bc-00000000000000a1",
        "claude-token:ac-00000000000000ag",
        "claude-token:ac-00000000000000a1-meta",
        "claude-token:inert@example.invalid",
        "claude-token:m29",
    ] {
        assert!(Want::parse(bad).is_err(), "{bad}");
    }
    assert!(
        Key::parse(&format!("{KEY}-meta")).is_none(),
        "a key never names the meta record"
    );
    for name in [
        format!("entry:aterm-claude-token-{KEY}"),
        format!("entry:aterm-claude-token-{KEY}-meta"),
    ] {
        assert!(Want::parse(&name).is_err(), "{name}");
    }
}

/// A Claude Code token has one producer, and `need give` refuses every word that names one,
/// its key valid or not, with words that name that producer. No other want has a producer, and
/// no other word, a request id among them, is refused so.
#[test]
fn a_claude_token_has_one_producer_and_is_never_given() {
    let want = Want::parse(&format!("claude-token:{KEY}")).unwrap();
    assert_eq!(want.producer(), Some(CLAUDE_TOKEN_PRODUCER));
    for word in [
        want.key(),
        "claude-token:".to_owned(),
        "claude-token:inert@example.invalid".to_owned(),
        format!("claude-token:{KEY}-meta"),
    ] {
        let why = Want::give_refusal(&word).unwrap_or_else(|| panic!("{word} is refused"));
        assert!(why.contains(CLAUDE_TOKEN_PRODUCER), "{word}: {why}");
    }
    for word in [
        "site-deployer-key",
        "notary",
        "gh:dev",
        "mailbox:m29",
        "machines-admin",
        "entry:notary",
        "login:claude/m29",
        "paper-master",
        "claude-tokens:ac-00000000000000a1",
        "0123456789abcdef0123456789abcdef",
    ] {
        assert!(Want::give_refusal(word).is_none(), "{word}");
        if let Ok(other) = Want::parse(word) {
            assert!(other.producer().is_none(), "{word}");
        }
    }
}
