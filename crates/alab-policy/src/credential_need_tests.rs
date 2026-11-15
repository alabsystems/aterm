// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn entry(name: &str, purpose: &str) -> VaultEntry {
    VaultEntry {
        name: name.to_owned(),
        purpose: purpose.to_owned(),
    }
}

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| (*l).to_owned()).collect()
}

/// The release machine's registry on 2026-11-12, as the host resolves the incident's wants.
fn held_notary() -> CredentialRead {
    CredentialRead::Resolved(vec![(
        "notary".to_owned(),
        Resolution::Held(vec![entry("release-notary", "the notary credential")]),
    )])
}

/// THE INCIDENT'S LINES ARE CREDENTIAL LINES, each with the want aterm resolves.
#[test]
fn credential_lines_name_their_wants() {
    let table: &[(&str, &[&str])] = &[
        ("the release credential", &[RELEASE_WANT]),
        (
            "the owner's release credential to publish 0.200.0",
            &[RELEASE_WANT],
        ),
        ("the notary credential", &["notary"]),
        ("an app-specific password for notarytool", &["notary"]),
        ("the Developer ID signing certificate", &[RELEASE_WANT]),
        ("a GitHub token with repo scope", &["gh:dev"]),
        ("the release GitHub token", &["gh:release"]),
        ("the site deployer's key", &["site-deployer-key"]),
        ("the Acme API key", &[]),
        ("the SSH key", &[]),
        ("the private key", &[]),
        ("the signing key", &[RELEASE_WANT]),
        ("the codesign key", &[RELEASE_WANT]),
        ("a GitHub key with repo scope", &["gh:dev"]),
        ("the service account key", &["site-deployer-key"]),
        ("the deployment keys", &["site-deployer-key"]),
        ("the mailbox password for ops", &[]),
    ];
    for (line, wants) in table {
        assert_eq!(wants_of(line).as_deref(), Some(*wants), "{line}");
    }
}

/// A CREDENTIAL IS NEVER A PERSON'S NEED: each credential line of a report is among its
/// credential needs ([`crate::needs::credential_needs`]) and off the person's list
/// ([`crate::needs::persons_needs`]), and the person's need beside it stays there alone.
#[test]
fn a_credential_line_is_no_persons_need() {
    let person = "approve the 0.200.0 release";
    for line in [
        "the release credential",
        "the notary credential",
        "a GitHub token with repo scope",
        "the Acme API key",
        "the mailbox password for ops",
    ] {
        let report = format!("NEEDS YOU: {line}\nNEEDS YOU: {person}");
        assert_eq!(
            crate::needs::persons_needs(&report),
            [person],
            "only the person's need is theirs: {report:?}"
        );
        assert_eq!(
            crate::needs::credential_needs(&report),
            [line],
            "the credential is resolved on the secrets path: {report:?}"
        );
    }
}

/// NEGATIVE CONTROLS: a person's decision, a grant, hardware, a release decision and a vendor
/// sign-in at a window are no credential line, so they stay a person's need.
#[test]
fn a_persons_need_is_no_credential_line() {
    for line in [
        "approve the 0.200.0 release",
        "decide whether to publish 1.5.0 to the public tier",
        "decide whether to cut the 1.5.0 release",
        "a decision on the signing policy",
        "the Full Disk Access grant",
        "plug in the YubiKey",
        "press the Caps Lock key to verify the mapping",
        "hold the Control key",
        "press the key to release the modifier",
        "touch the security key to sign in",
        "touch the security key while an API request waits",
        "confirm the shortcut key for deployment",
        "choose the primary key for this database",
        "a person's sign-in to the agent's app in a window",
        "approve the worker's sign-in for ops: run `aterm harness accounts login ops`",
    ] {
        assert_eq!(wants_of(line), None, "{line}");
        assert_eq!(
            super::super::needs::persons_needs(&format!("NEEDS YOU: {line}")),
            vec![line.to_owned()],
            "a real person's need stays visible: {line}"
        );
    }
}

/// FOUND: the line names the entry, how to read it and its purpose, and the release machine for a
/// release need; nothing is filed for a want the vault holds.
#[test]
fn a_held_want_is_named_with_how_to_read_it() {
    let named = lines(&["the notary credential to cut the release"]);
    let (text, keys) = told_line(&named, &held_notary(), &BTreeSet::new()).expect("told");
    assert!(text.starts_with(CREDENTIAL_HEAD), "{text}");
    assert!(
        text.contains(
            "`release-notary` (`aterm secrets get release-notary`; the notary credential)"
        ),
        "{text}"
    );
    assert!(text.contains(RELEASE_MACHINE), "{text}");
    assert!(!text.contains("filed"), "{text}");
    assert!(text.ends_with(CREDENTIAL_TAIL), "{text}");
    assert_eq!(keys, ["held:release-notary", RELEASE_WANT]);
}

/// MISSING: a want the vault does not hold was filed, and the line says so; one the host could
/// not file says why; one no host resolved, or the vocabulary cannot name, is the agent's to file.
#[test]
fn a_missing_want_is_filed_never_asked_of_a_person() {
    let read = CredentialRead::Resolved(vec![
        ("gh:dev".to_owned(), Resolution::Filed),
        (
            "site-deployer-key".to_owned(),
            Resolution::Failed("ledger locked".into()),
        ),
    ]);
    let named = lines(&["a GitHub token", "the site deployer's key", "the npm token"]);
    let (text, _) = told_line(&named, &read, &BTreeSet::new()).expect("told");
    assert!(
        text.contains("aterm filed `aterm secrets need gh:dev`."),
        "{text}"
    );
    assert!(
        text.contains("could not file `aterm secrets need site-deployer-key` (ledger locked)"),
        "{text}"
    );
    assert!(
        text.contains("File it: `aterm secrets need <want>`."),
        "{text}"
    );
    assert!(!text.contains("NEEDS YOU:"), "{text}");
    let (text, _) = told_line(
        &lines(&["the notary credential"]),
        &CredentialRead::NotAsked,
        &BTreeSet::new(),
    )
    .expect("told");
    assert!(text.contains("`aterm secrets need notary`"), "{text}");
}

/// ONCE A FACT: what was told is never told again; a new fact beside it is told alone; a line
/// that names no credential tells nothing.
#[test]
fn each_fact_is_told_once() {
    let named = lines(&["the notary credential"]);
    let (_, keys) = told_line(&named, &held_notary(), &BTreeSet::new()).expect("told");
    let told: BTreeSet<String> = keys.into_iter().collect();
    assert_eq!(told_line(&named, &held_notary(), &told), None);
    let (text, keys) = told_line(
        &lines(&["the notary credential", "the release credential"]),
        &held_notary(),
        &told,
    )
    .expect("the release machine is new");
    assert_eq!(keys, [RELEASE_WANT]);
    assert!(!text.contains("release-notary"), "{text}");
    assert_eq!(
        told_line(
            &lines(&["a decision on the schema"]),
            &held_notary(),
            &BTreeSet::new()
        ),
        None
    );
}

/// The host resolves each want once, never the release machine's.
#[test]
fn the_host_resolves_each_want_once() {
    assert_eq!(
        host_wants(&lines(&[
            "the release credential",
            "the notary credential",
            "an app-specific password for notarytool",
            "a GitHub token",
        ])),
        ["notary", "gh:dev"]
    );
}
