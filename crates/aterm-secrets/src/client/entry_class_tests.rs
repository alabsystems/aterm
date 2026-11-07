// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The entry classes through a real log vault (`docs/DESIGN-claude-setup-token-2026-11-05.md`
//! §3.1, §3.2, §4.4): what a member's `Put` makes of a file, and the Claude Code token class kept
//! out of every text an admin's publish writes.

use super::log_entries_tests::two_macs;
use super::*;

const TOKEN: &str = "aterm-claude-token-ac-0123456789abcdef";
const META: &str = "aterm-claude-token-ac-0123456789abcdef-meta";
const FIXTURE: &[u8] = b"inert-claude-fixture-value";

/// `name` in `[everyone]`, as the Mac at `paths` opens its log.
fn in_log(paths: &Paths, name: &str) -> Option<Vec<u8>> {
    let identity = read_identity(paths).unwrap().unwrap();
    let records = load_records(paths).unwrap();
    let opened = records
        .log()
        .unwrap()
        .open_audience_entries(&identity)
        .unwrap()?;
    opened
        .by_audience
        .get(&Audience::Everyone)?
        .get(name)
        .map(|entry| entry.value.as_slice().to_vec())
}

/// What a sync of `paths` applied.
fn applied(paths: &Paths) -> Report {
    match sync(paths, None).expect("sync") {
        SyncOutcome::Applied { report, .. } => report,
        outcome => panic!("a sync applies the new records: {outcome:?}"),
    }
}

/// A member's `Put` of a home path aterm does not name never becomes a file on another Mac,
/// and is reported refused there. NEGATIVE CONTROL: the same value under a shared name, put the
/// same way, is written.
#[test]
fn a_set_apart_put_is_never_written_and_a_shared_one_is() {
    const SET_APART: [&str; 4] = [
        "~/.zshrc",
        "~/Library/LaunchAgents/x.plist",
        "~/.ssh/config",
        "~/.config/git/hooks/x",
    ];
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    for name in SET_APART.into_iter().chain(["shared-control"]) {
        test_put_everyone(m22, name, b"runs on every Mac").expect("m22 puts");
    }
    let report = applied(m7);
    for name in SET_APART {
        let path = name
            .strip_prefix("~/")
            .map(|rel| m7.home.join(rel))
            .unwrap();
        assert!(!path.exists(), "{name}");
        assert!(in_log(m7, name).is_some(), "CONTROL: m7 reads {name}");
    }
    let mut refused: Vec<&str> = report
        .refused
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    refused.sort_unstable();
    let mut want = SET_APART;
    want.sort_unstable();
    assert_eq!(refused, want, "{report:?}");
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/shared-control")).unwrap(),
        b"runs on every Mac",
        "CONTROL: a shared name is written"
    );
}

/// A Claude Code token entry, put in the log, never becomes a file, is read by no entry reader
/// that `aterm secrets get` uses, and appears in no text: not the admin's document, its render
/// (the admin locker's text and the edit draft) or any locker. An admin's publish of a text
/// without it deletes nothing, and a text with one typed in is refused. CONTROL: a shared entry
/// put the same way is in each of them.
#[test]
fn a_claude_token_entry_is_no_file_and_in_no_text() {
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    test_put_everyone(m22, TOKEN, FIXTURE).expect("m22 puts the token");
    test_put_everyone(m22, META, b"{}").expect("m22 puts its record");
    test_put_everyone(m22, "shared-control", b"control-value").expect("m22 puts a shared entry");
    for paths in [m22, m7] {
        let _ = applied(paths);
        assert!(!paths.home.join(".secrets").join(TOKEN).exists());
        assert!(!paths.home.join(".secrets").join(META).exists());
        assert!(
            paths.home.join(".secrets/shared-control").exists(),
            "CONTROL"
        );
        let config = config(paths).unwrap().unwrap();
        assert!(
            read_verified_entry(paths, &config, TOKEN).is_err(),
            "`get` refuses it"
        );
    }

    let admin = admin_open(m22).expect("m22 opens the vault");
    let rendered = admin.doc.render();
    let holds = |text: &[u8], value: &[u8]| text.windows(value.len()).any(|w| w == value);
    assert!(holds(rendered.as_slice(), b"control-value"), "CONTROL");
    for name in [TOKEN.as_bytes(), META.as_bytes(), FIXTURE] {
        assert!(!holds(rendered.as_slice(), name), "the admin's text");
    }
    assert!(admin.doc.entry(&Audience::Everyone, TOKEN).is_none());
    for plan in admin.doc.lockers() {
        assert!(!holds(plan.text.as_slice(), TOKEN.as_bytes()), "a locker");
    }

    // A publish of a text without the token deletes nothing.
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "another", b"v").unwrap();
    admin_publish(m22, &admin, &doc).expect("the publish");
    assert_eq!(in_log(m22, TOKEN).as_deref(), Some(FIXTURE));
    assert_eq!(in_log(m22, META).as_deref(), Some(&b"{}"[..]));
    assert!(
        in_log(m22, "another").is_some(),
        "CONTROL: the publish landed"
    );

    // A text with one typed in is refused, and the log keeps the token as it was.
    let admin = admin_open(m22).expect("m22 opens the vault");
    let mut doc = admin.doc.clone();
    assert!(doc.set_entry(&Audience::Everyone, TOKEN, b"x").is_err());
    let mut text = String::from_utf8(admin.doc.render().as_slice().to_vec()).unwrap();
    text.push_str(&format!("[everyone]\n{TOKEN}=typed-in\n"));
    let typed = Document::parse(&text).expect("a text parses");
    let error = admin_publish(m22, &admin, &typed).expect_err("refused");
    assert!(error.0.contains("Claude Code token"), "{error}");
    assert_eq!(in_log(m22, TOKEN).as_deref(), Some(FIXTURE));
    assert!(doc.added_refusal(&admin.doc).is_none());
    assert!(
        typed.added_refusal(&admin.doc).is_some(),
        "an edit refuses it too"
    );
}
