// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn roster_only_requires_existing_authority_and_preserves_newer_state() {
    use ring::signature::KeyPair as _;
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let repo = scratch.path().join("repo");
    let input = scratch.path().join("public/aterm-machines.toml");
    let kept = home.join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    let key = home.join(atpkg_keys::provision::MACHINE_KEY_REL);
    let identity = machines::identity_path_in(&home);
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let pkcs8 =
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let pubkey = aterm_codec::base64::encode(pair.public_key().as_ref()).unwrap();
    let master = seed().pubkey_b64().unwrap();
    let member = || Machine {
        id: "m3".into(),
        pubkey: pubkey.clone(),
        added_at: "2026-01-01T00:00:00Z".into(),
        not_after: None,
    };
    let write = |path: &Path, roster: Roster| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = roster.to_toml().unwrap().into_bytes();
        std::fs::write(path, &bytes).unwrap();
        std::fs::write(
            machines::RosterDocument::signature_path(path),
            seed().sign(&bytes).unwrap(),
        )
        .unwrap();
    };
    let run =
        |id, check| roster_only_in(&repo, &home, id, check, &input, &[&master], 1_793_000_000);
    write(&input, roster_with(3, vec![member()], vec![]));
    assert!(run("m3", false).is_err(), "missing key must never mint");
    assert!(!kept.exists());
    std::fs::write(&key, pkcs8.as_ref()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let identity_bytes = format!("id = \"m3\"\npubkey = \"{pubkey}\"\n");
    std::fs::write(&identity, &identity_bytes).unwrap();
    assert!(run("m4", false).is_err());
    assert!(!kept.exists());
    assert_eq!(run("m3", true).unwrap().roster_seq, 3);
    assert!(
        !kept.parent().unwrap().exists(),
        "check must not create a lock directory"
    );
    assert_eq!(run("m3", false).unwrap().roster_seq, 3);
    let original = machines::RosterDocument::read(&kept).unwrap();
    std::fs::write(&identity, "id = \"m3\"\npubkey = \"wrong\"\n").unwrap();
    assert!(run("m3", false).is_err());
    std::fs::write(&identity, &identity_bytes).unwrap();
    std::fs::remove_file(machines::RosterDocument::signature_path(&input)).unwrap();
    assert!(run("m3", false).is_err(), "unsigned input must refuse");
    assert_eq!(machines::RosterDocument::read(&kept).unwrap(), original);
    let mut renamed = member();
    renamed.id = "m4".into();
    write(&input, roster_with(4, vec![renamed], vec![]));
    assert!(
        run("m3", false).is_err(),
        "the same key under a different id must refuse"
    );
    assert_eq!(
        machines::RosterDocument::read(&kept).unwrap(),
        machines::RosterDocument::read(&input).unwrap()
    );
    write(&input, roster_with(5, vec![], vec!["m3".into()]));
    assert!(run("m3", false).is_err());
    let denied = machines::RosterDocument::read(&input).unwrap();
    assert_eq!(machines::RosterDocument::read(&kept).unwrap(), denied);
    write(&input, roster_with(3, vec![member()], vec![]));
    assert!(
        run("m3", false).is_err(),
        "the old grant cannot revive the denied signer"
    );
    assert_eq!(machines::RosterDocument::read(&kept).unwrap(), denied);
    let mut expired = roster_with(6, vec![member()], vec![]);
    expired.valid_until = "2026-01-01T00:00:00Z".into();
    write(&input, expired);
    assert!(run("m3", false).is_err());
    assert_eq!(
        machines::RosterDocument::read(&kept).unwrap(),
        machines::RosterDocument::read(&input).unwrap()
    );
    write(&input, roster_with(3, vec![member()], vec![]));
    write(&kept, roster_with(7, vec![member()], vec![]));
    let newer = machines::RosterDocument::read(&kept).unwrap();
    assert_eq!(run("m3", false).unwrap().roster_seq, 7);
    assert_eq!(machines::RosterDocument::read(&kept).unwrap(), newer);
    write(&input, roster_with(7, vec![], vec!["m3".into()]));
    assert!(
        run("m3", false).is_err(),
        "same-generation forks must refuse"
    );
    assert_eq!(machines::RosterDocument::read(&kept).unwrap(), newer);
    write(&input, roster_with(3, vec![member()], vec![]));
    write(&kept, roster_with(8, vec![], vec!["m3".into()]));
    let revoked = machines::RosterDocument::read(&kept).unwrap();
    assert!(
        run("m3", false).is_err(),
        "newer kept revocation is effective"
    );
    assert_eq!(machines::RosterDocument::read(&kept).unwrap(), revoked);
    assert_eq!(std::fs::read(&key).unwrap(), pkcs8.as_ref());
    assert_eq!(std::fs::read_to_string(identity).unwrap(), identity_bytes);
    assert!(
        !home
            .join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL)
            .exists()
    );
    assert!(!repo.join("dist").exists());
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1);
}

/// `--check` and a dry release promise no writes, so they only look at the release
/// keychain; a provision run and a real release's audit may re-assert its partition list.
#[cfg(unix)]
#[test]
fn a_check_only_looks_at_the_release_keychain() {
    use crate::release_keychain::Leave;
    assert_eq!(leave(true), Leave::Look);
    assert_eq!(leave(false), Leave::Write);
}

/// The keychain line follows its `leave`: a look (the line's, and the pinned identity's
/// proof) opens a held identity's keychain and writes nothing else, and runs no codesign
/// even with its key's partition list gone; a write re-asserts the partition list and
/// test-signs.
#[cfg(unix)]
#[test]
fn the_keychain_line_looks_or_test_signs_as_told() {
    use crate::release_keychain::fake::{Fake, World};
    use crate::release_keychain::{Keychains, Leave, OPENED_NOTE, login_keychain_path};
    let sha1 = "D".repeat(40);
    let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
    let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
    let mut world = World::new(&sha1);
    world.search_list = vec![login_keychain_path(&home)];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect("install");
    let release = kc.release();
    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&release)
        .unwrap()
        .partition = false;
    fake.0.borrow_mut().log.clear();

    let Check::Pass(line) = keychain_check_in(&home, "m9", &fake, Leave::Look) else {
        panic!("a held identity is ready to a look");
    };
    assert!(line.contains(OPENED_NOTE), "{line}");
    assert_eq!(
        crate::apple::pinned_identity_proves(&kc, &sha1, Leave::Look),
        Ok(crate::release_keychain::Proof::Opened)
    );
    let changed: Vec<_> = fake
        .writes()
        .into_iter()
        .filter(|c| c[1] != "unlock-keychain")
        .collect();
    assert!(changed.is_empty(), "a look only unlocks: {changed:?}");
    assert!(fake.calls("codesign", "--force").is_empty());

    let Check::Pass(line) = keychain_check_in(&home, "m9", &fake, Leave::Write) else {
        panic!("a write re-asserts and signs");
    };
    assert!(line.contains("a test signature passed"), "{line}");
    assert!(fake.0.borrow().keychains[&release].partition);
}

/// The audit's shim line is the build's own refusal, over the `rustup` row's answer: a
/// symlinked cargo beside it (how one machine's rustup linked it) fails with the build's
/// words, the same proxy as a hard link (how the row's copy lays it) passes, and no
/// rustup at all fails naming the row's provider. A failing copy's fix removes and
/// re-places it; a failing rustup on `$PATH` (Homebrew's link) is fixed by placing the
/// copy that outranks it, never by editing that directory.
#[cfg(unix)]
#[test]
fn a_symlinked_cargo_proxy_fails_the_audit_as_it_fails_the_build() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = aterm_tempfile::Builder::new()
        .prefix("provision-shims-")
        .tempdir()
        .unwrap();
    let bin = home.path().join("tooling.noindex/rustup-1.29.2/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(bin.join("rustup"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(bin.join("rustup"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("rustup", bin.join("cargo")).unwrap();
    let tooling = home.path().join("tooling.noindex");
    let verdict = || rust_shim_verdict(Ok(bin.join("rustup")), Some(&tooling));
    match verdict() {
        Check::Fail { what, fix } => {
            assert!(what.contains("not a real regular file"), "{what}");
            assert!(
                fix.starts_with(&format!(
                    "remove {}, the row's copy",
                    tooling.join("rustup-1.29.2").display()
                )),
                "{fix}"
            );
            assert_eq!(fix.matches("verify provision").count(), 1, "{fix}");
        }
        other => panic!("a symlinked cargo passed the audit: {other:?}"),
    }
    std::fs::remove_file(bin.join("cargo")).unwrap();
    std::fs::hard_link(bin.join("rustup"), bin.join("cargo")).unwrap();
    assert!(matches!(verdict(), Check::Pass(_)), "{:?}", verdict());
    let row = aterm_uds::host::tool("rustup");
    let on_path = home.path().join("opt/homebrew/bin");
    std::fs::create_dir_all(&on_path).unwrap();
    std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::copy(bin.join("rustup"), on_path.join("rustup")).unwrap();
    std::os::unix::fs::symlink("rustup", on_path.join("cargo")).unwrap();
    match rust_shim_verdict(Ok(on_path.join("rustup")), Some(&tooling)) {
        Check::Fail { what, fix } => {
            assert!(what.contains("not a real regular file"), "{what}");
            assert!(fix.starts_with(&row.remedy()), "the row's provider: {fix}");
            assert!(
                !fix.contains("ln -f") && !fix.contains("remove"),
                "nothing is changed in another's directory: {fix}"
            );
        }
        other => panic!("a symlinked cargo on $PATH passed the audit: {other:?}"),
    }
    let absent = row
        .locate_in(&aterm_uds::host::Lookup {
            named: None,
            tooling: Some(home.path().join("empty")),
            path: None,
        })
        .unwrap_err();
    match rust_shim_verdict(Err(absent), Some(&tooling)) {
        Check::Fail { what, fix } => {
            assert!(what.starts_with("no rustup (looked in"), "{what}");
            assert_eq!(fix, row.remedy());
        }
        other => panic!("no rustup passed the audit: {other:?}"),
    }
}

#[test]
fn notary_answers_are_classified_by_every_line_they_carry() {
    let agreement = "Error: HTTP status code: 403. A required agreement is missing or has \
                     expired. This request requires an in-effect agreement that has not been \
                     signed or has expired.";
    match notary_verdict(
        "aterm-notary",
        false,
        "",
        &format!("Fetching history\n{agreement}\n"),
    ) {
        Check::Owner { fix, .. } => {
            assert!(fix.contains("developer.apple.com"), "{fix}");
        }
        other => panic!("{other:?}"),
    }
    for credential in [
        "Error: HTTP status code: 401. Invalid credentials. Username or password is incorrect.",
        "Error: No Keychain password item found for profile: aterm-notary",
    ] {
        match notary_verdict("aterm-notary", false, "", credential) {
            Check::Fail { fix, .. } => {
                assert!(fix.contains("aterm secrets need notary"), "{fix}");
            }
            other => panic!("{credential}: {other:?}"),
        }
    }
    match notary_verdict(
        "aterm-notary",
        false,
        "",
        "Error: The Internet connection appears to be offline.",
    ) {
        Check::CouldNotRun(_) => {}
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        notary_verdict("aterm-notary", true, "No submission history.", ""),
        Check::Pass(_)
    ));
    assert_eq!(
        audited(Check::Owner {
            what: "w".into(),
            fix: "f".into()
        }),
        Audited::Gap {
            what: "w".into(),
            fix: "f".into(),
            owner: true
        }
    );
    assert_eq!(
        audited(Check::Fail {
            what: "w".into(),
            fix: "f".into()
        }),
        Audited::Gap {
            what: "w".into(),
            fix: "f".into(),
            owner: false
        }
    );
    assert_eq!(
        audited(Check::Todo {
            what: "w".into(),
            next: "n".into()
        }),
        Audited::Gap {
            what: "w".into(),
            fix: "n".into(),
            owner: false
        },
        "a credential waiting at Apple fails the step; the owner is not asked"
    );
}

#[test]
fn a_missing_credential_is_filed_with_the_lab_not_asked_of_a_person() {
    use aterm_secrets::need;

    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(dir.path());
    let source = dir.path().join("filed.vault");
    aterm_secrets::client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1"))
        .unwrap();
    need::test_alab_policy(&paths);
    let missing = channel_token_missing(Some(Path::new("/nowhere/gh_access_token_alabsystems")));
    let refused = notary_verdict("notary", false, "", "No Keychain password item found");
    for (check, want, purpose) in [
        (missing, Want::Gh(GhProfile::Release), Purpose::AghRelease),
        (refused, Want::Notary, Purpose::ReleaseNotary),
    ] {
        let key = want.key();
        let check = filed(check, Ok(paths.clone()), want.clone(), purpose);
        let Check::Fail { fix, .. } = &check else {
            panic!("{check:?}");
        };
        assert!(
            fix.starts_with(&format!("filed `aterm secrets need {key}` with the lab")),
            "{fix}"
        );
        assert!(
            matches!(audited(check), Audited::Gap { owner: false, .. }),
            "a missing credential fails the step; no owner ask is raised"
        );
        let requests = need::peek(&paths).unwrap();
        let request = requests
            .iter()
            .find(|n| n.want == want && n.purpose == purpose)
            .expect("filed in the ledger");
        assert!(!request.state.ended());
    }
}

#[test]
fn channel_token_file_admission_preserves_regular_rules_and_refuses_aliases_before_reading() {
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let regular = dir.path().join("synthetic-token");
    let check = |path: Option<&Path>, present, may_change| {
        channel_token_check_at(may_change, path, || present)
    };
    assert!(check(None, false, false).is_none());
    assert!(check(Some(&regular), false, false).is_none());
    std::fs::write(&regular, b"synthetic-token-only").unwrap();
    std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        check(Some(&regular), true, false),
        Some(Check::Pass(_))
    ));
    assert!(check(Some(&regular), false, false).is_none());
    std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        matches!(check(Some(&regular), true, false), Some(Check::Fail { .. })),
        "a file mode is this machine's"
    );
    assert!(matches!(
        check(Some(&regular), true, true),
        Some(Check::Pass(_))
    ));
    assert_eq!(
        std::fs::metadata(&regular).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o644)).unwrap();

    let alias = dir.path().join("linked-token");
    std::os::unix::fs::symlink(&regular, &alias).unwrap();
    assert!(
        aterm_update_core::github_rest::Token::from_file(&alias)
            .unwrap()
            .is_some(),
        "the real token reader follows this synthetic fixture alias"
    );
    for may_change in [false, true] {
        let calls = Cell::new(0);
        let result = channel_token_check_at(may_change, Some(&alias), || {
            calls.set(calls.get() + 1);
            aterm_update_core::github_rest::Token::from_file(&alias)
                .unwrap()
                .is_some()
        });
        assert!(matches!(result, Some(Check::Fail { .. })), "{result:?}");
        assert_eq!(
            calls.get(),
            0,
            "unadmitted paths must not reach the token reader"
        );
        assert_eq!(
            std::fs::metadata(&regular).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
    assert_eq!(std::fs::read(&regular).unwrap(), b"synthetic-token-only");
    let missing = dir.path().join("missing");
    let dangling = dir.path().join("dangling-token");
    std::os::unix::fs::symlink(&missing, &dangling).unwrap();
    for path in [dangling.as_path(), dir.path()] {
        let result = channel_token_check_at(false, Some(path), || {
            panic!("an unadmitted token path must not be read")
        });
        assert!(matches!(result, Some(Check::Fail { .. })), "{result:?}");
    }
}

#[test]
fn every_owner_ask_is_one_the_wire_stands() {
    let w = agreement_ask();
    assert_eq!(w.key, "release.notary");
    assert!(
        w.reason.chars().count() <= aterm_messages::need::REASON_CAP,
        "{w:?}"
    );
    assert!(
        w.step.chars().count() <= aterm_messages::need::STEP_CAP,
        "{w:?}"
    );
    let line = format!(
        "ask {} tag=release {} -- {} -- {}",
        w.key, w.title, w.reason, w.step
    );
    if let Err(e) = aterm_messages::wire::NoticeRequest::parse(&line) {
        panic!("{line}: {e}");
    }
}

#[test]
fn notary_credential_failures_raise_the_typed_need_and_live_success_closes_it() {
    use aterm_secrets::need;

    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(dir.path());
    let source = dir.path().join("notary.vault");
    aterm_secrets::client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1"))
        .unwrap();
    need::test_alab_policy(&paths);
    let refused = notary_probe_verdict(
        "notary",
        false,
        "",
        "No Keychain password item found for profile: notary.",
    );
    assert_eq!(refused.need, NotaryNeed::Raise);
    filed(
        refused.check,
        Ok(paths.clone()),
        Want::Notary,
        Purpose::ReleaseNotary,
    );
    let requests = need::peek(&paths).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].want, Want::Notary);
    assert_eq!(requests[0].purpose, Purpose::ReleaseNotary);
    assert!(!requests[0].state.ended());

    for failure in [
        "HTTP 403: the developer agreement is missing",
        "The network connection timed out",
    ] {
        let probe = notary_probe_verdict("notary", false, "", failure);
        assert_eq!(probe.need, NotaryNeed::Unchanged);
    }
    let ready = notary_probe_verdict("notary", true, "No submission history.", "");
    assert_eq!(ready.need, NotaryNeed::Satisfied);
    close_notary_need(&paths).unwrap();
    assert!(need::peek(&paths).unwrap()[0].state.ended());
}

#[test]
fn notary_only_check_and_existing_success_never_materialize_credentials() {
    let checked = notary_only_with(
        true,
        || notary_probe_verdict("notary", false, "", "No Keychain password item found"),
        || panic!("a check must not materialize credentials"),
    );
    assert!(!matches!(checked, Check::Pass(_)));
    let ready = notary_only_with(
        false,
        || notary_probe_verdict("notary", true, "No submission history.", ""),
        || panic!("an authenticating profile must remain unchanged"),
    );
    assert!(matches!(ready, Check::Pass(_)));
}

#[test]
fn notary_only_repair_requires_an_available_credential_and_a_fresh_provider_check() {
    let mut probes = 0;
    let mut materialized = 0;
    let ready = notary_only_with(
        false,
        || {
            probes += 1;
            if probes == 1 {
                notary_probe_verdict("notary", false, "", "No Keychain password item found")
            } else {
                notary_probe_verdict("notary", true, "No submission history.", "")
            }
        },
        || {
            materialized += 1;
            Ok(Some("authenticated shared credential".into()))
        },
    );
    assert!(matches!(ready, Check::Pass(_)));
    assert_eq!((probes, materialized), (2, 1));
    for available in [false, true] {
        let not_ready = notary_only_with(
            false,
            || notary_probe_verdict("notary", false, "", "No Keychain password item found"),
            || Ok(available.then(|| "materialized".into())),
        );
        assert!(!matches!(not_ready, Check::Pass(_)));
    }
    let failed = notary_only_with(
        false,
        || notary_probe_verdict("notary", false, "", "No Keychain password item found"),
        || Err("credential unavailable".into()),
    );
    assert!(matches!(failed, Check::Fail { .. }));
}

#[test]
fn notary_acquire_stores_missing_credential_when_mutable_and_rechecks_live() {
    let mut probes = 0usize;
    let mut ensured = 0usize;
    let acquired = notary_acquire_with(
        true,
        || {
            probes += 1;
            if probes == 1 {
                notary_probe_verdict(
                    "notary",
                    false,
                    "",
                    "No Keychain password item found for profile: notary.",
                )
            } else {
                notary_probe_verdict("notary", true, "No submission history.", "")
            }
        },
        |may_change| {
            assert!(may_change);
            ensured += 1;
            crate::apple::Outcome::Ready {
                ids: Vec::new(),
                note: None,
            }
        },
    );
    assert_eq!(ensured, 1);
    assert_eq!(probes, 2);
    assert_eq!(acquired.need, NotaryNeed::Satisfied);
    assert!(matches!(acquired.check, Check::Pass(_)));

    let check_only = notary_acquire_with(
        false,
        || {
            notary_probe_verdict(
                "notary",
                false,
                "",
                "No Keychain password item found for profile: notary.",
            )
        },
        |_| panic!("--check must not invoke ensure_notary"),
    );
    assert_eq!(check_only.need, NotaryNeed::Raise);
    assert!(matches!(check_only.check, Check::Fail { .. }));

    for unacquirable in [
        notary_probe_verdict(
            "notary",
            false,
            "",
            "HTTP 403: the developer agreement is missing",
        ),
        notary_probe_verdict("notary", false, "", "The network connection timed out"),
        NotaryProbe {
            check: Check::Fail {
                what: "could not run xcrun notarytool".into(),
                fix: "install CLT".into(),
            },
            need: NotaryNeed::Unchanged,
        },
    ] {
        let mut item = Some(unacquirable);
        let outcome = notary_acquire_with(
            true,
            || item.take().expect("probed once"),
            |_| panic!("non-credential failure must not invoke ensure_notary"),
        );
        assert_eq!(outcome.need, NotaryNeed::Unchanged);
    }
}
use crate::machines::current_roster::{public_vault_candidate, require_roster_floor};
use aterm_update_core::roster::{Machine, Roster, SUPPORTED_SCHEMA};

fn seed() -> atpkg_keys::master::MasterSeed {
    let phrase = "2".repeat(52);
    atpkg_keys::master::parse_master(&phrase)
        .expect("synthetic phrase")
        .seed()
}

fn pubkey_of(byte: u8) -> String {
    aterm_codec::base64::encode(&[byte; 32]).expect("32-byte key")
}

fn roster_with(seq: u64, machines: Vec<Machine>, revoked: Vec<String>) -> Roster {
    Roster {
        schema: SUPPORTED_SCHEMA,
        roster_seq: seq,
        valid_until: "9999-12-31T00:00:00Z".into(),
        machines,
        revoked,
    }
}

fn machine(id: &str, key_byte: u8) -> Machine {
    Machine {
        id: id.into(),
        pubkey: pubkey_of(key_byte),
        added_at: "2026-01-01T00:00:00Z".into(),
        not_after: None,
    }
}

fn signed_candidate(seq: u64) -> (String, Vec<u8>, Vec<u8>) {
    signed_candidate_with(seq, vec![machine("m3", 0x42)])
}

fn signed_candidate_with(seq: u64, machines: Vec<Machine>) -> (String, Vec<u8>, Vec<u8>) {
    let s = seed();
    let r = roster_with(seq, machines, vec![]);
    let bytes = r.to_toml().expect("valid roster").into_bytes();
    let sig = s.sign(&bytes).expect("sign");
    (s.pubkey_b64().expect("pubkey"), bytes, sig)
}

fn newest_fixture_candidate(paths: &[&Path], masters: &[&str]) -> Result<Option<Candidate>> {
    let mut candidates = Vec::new();
    for path in paths {
        candidates.extend(read_local_candidate_with(path, masters)?);
    }
    machines::current_roster::select_all(candidates)
}

#[test]
fn the_release_roster_audit_refuses_authority_below_its_retained_floor() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let dist = home.path().join("dist/aterm-machines.toml");
    let kept = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    let (master, bytes, sig) = signed_candidate(3);
    let grant = admit_candidate(&[&master], bytes, sig).unwrap();
    promote_public_pair(&dist, &grant, &[&master]).unwrap();
    promote_public_pair(&kept, &grant, &[&master]).unwrap();
    let audit = || {
        audit_roster_at(
            home.path(),
            &[&dist, &kept],
            &[&master],
            Ok(grant.clone()),
            &pubkey_of(0x42),
            1_793_000_000,
        )
    };
    assert_eq!(audit().unwrap().roster_seq, 3);
    aterm_uds::private_file::ensure_private_dir(&paths.dir).unwrap();
    aterm_uds::private_file::write_private(&paths.dir, "release-ring-roster-floor", b"5\n")
        .unwrap();
    assert!(
        audit().is_err(),
        "an old grant must not bypass the retained roster floor"
    );
}

#[test]
fn ordinary_provisioning_selects_a_newer_kept_roster_beside_existing_dist() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dist = scratch.path().join("dist/aterm-machines.toml");
    let kept = scratch.path().join("kept/aterm-machines.toml");
    let write = |path: &Path, seq| {
        let (master, bytes, sig) = signed_candidate(seq);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::write(machines::RosterDocument::signature_path(path), sig).unwrap();
        master
    };
    let master = write(&dist, 3);
    assert_eq!(
        newest_fixture_candidate(&[&dist, &kept], &[&master])
            .unwrap()
            .unwrap()
            .roster
            .roster_seq,
        3
    );
    write(&kept, 2);
    assert_eq!(
        newest_fixture_candidate(&[&dist, &kept], &[&master])
            .unwrap()
            .unwrap()
            .roster
            .roster_seq,
        3
    );
    write(&kept, 5);
    let local = newest_fixture_candidate(&[&dist, &kept], &[&master]).unwrap();
    let (_, bytes, sig) = signed_candidate(3);
    let fetched = admit_candidate(&[&master], bytes, sig).unwrap();
    let selected =
        machines::current_roster::select_all(local.into_iter().chain([fetched]).collect())
            .unwrap()
            .unwrap();
    assert_eq!(selected.roster.roster_seq, 5);
    promote_public_pair(&dist, &selected, &[&master]).unwrap();
    require_public_pair(&dist, &selected, &[&master]).unwrap();
}

#[test]
fn authenticated_public_pair_input_is_admitted_before_real_promotion() {
    use aterm_secrets::client::{Paths, PublicRosterPair, PublicRosterRead};
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let dist = home.path().join("dist/aterm-machines.toml");
    let kept = home.path().join("kept/aterm-machines.toml");
    let (master, raw, sig) = signed_candidate(3);
    let old = admit_candidate(&[&master], raw, sig).unwrap();
    promote_public_pair(&dist, &old, &[&master]).unwrap();
    promote_public_pair(&kept, &old, &[&master]).unwrap();
    let (_, raw, sig) = signed_candidate(5);
    let read = || {
        PublicRosterRead::Pair(PublicRosterPair {
            roster: raw.clone(),
            signature: sig.clone(),
        })
    };
    let (candidate, floor) = public_vault_candidate(&paths, &[&master], |_| Ok(read())).unwrap();
    assert_eq!(floor, 0);
    let local = newest_fixture_candidate(&[&dist, &kept], &[&master]).unwrap();
    let selected =
        machines::current_roster::select_all(local.into_iter().chain(candidate).collect())
            .unwrap()
            .unwrap();
    assert_eq!(selected.roster.roster_seq, 5);
    promote_public_pair(&kept, &selected, &[&master]).unwrap();
    promote_public_pair(&dist, &selected, &[&master]).unwrap();
    require_public_pair(&kept, &selected, &[&master]).unwrap();
    require_public_pair(&dist, &selected, &[&master]).unwrap();
    assert!(public_vault_candidate(&paths, &["not a master"], |_| Ok(read())).is_err());
    assert!(
        public_vault_candidate(&paths, &[&master], |_| Err(aterm_secrets::Error::new(
            "unavailable"
        )))
        .is_err()
    );
    aterm_uds::private_file::ensure_private_dir(&paths.dir).unwrap();
    aterm_uds::private_file::write_private(&paths.dir, "release-ring-roster-floor", b"7\n")
        .unwrap();
    let (_, floor) = public_vault_candidate(&paths, &[&master], |_| Ok(read())).unwrap();
    assert_eq!(floor, 7);
    assert!(require_roster_floor(&selected, floor).is_err());
    require_public_pair(&kept, &selected, &[&master]).unwrap();
    aterm_uds::private_file::write_private(&paths.dir, "release-ring-roster-floor", b"invalid")
        .unwrap();
    assert!(public_vault_candidate(&paths, &[&master], |_| Ok(read())).is_err());
}

#[test]
fn final_pair_read_refuses_a_committed_transaction_even_while_old_bytes_match() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("aterm-machines.toml");
    let (master, bytes, sig) = signed_candidate(3);
    let old = admit_candidate(&[&master], bytes, sig).unwrap();
    promote_public_pair(&path, &old, &[&master]).unwrap();
    let (_, bytes, sig) = signed_candidate(5);
    let next = admit_candidate(&[&master], bytes, sig).unwrap();
    let lock = lock_roster_pair(&path).unwrap();
    let expected = pair_premise(&path).unwrap();
    let committed = atpkg_keys::provision::commit_roster_pair(
        &lock,
        &utf8_path(&path).unwrap(),
        expected.as_ref(),
        &next.bytes,
        &next.sig,
    )
    .unwrap();
    drop(lock);
    assert_eq!(std::fs::read(&path).unwrap(), old.bytes);
    assert!(require_public_pair(&path, &old, &[&master]).is_err());
    drop(committed);
    drop(lock_roster_pair(&path).unwrap());
    require_public_pair(&path, &next, &[&master]).unwrap();
    assert!(require_public_pair(&path, &old, &[&master]).is_err());
}

#[test]
fn changed_authority_refuses_a_planned_mint_before_any_publication() {
    use aterm_secrets::client::{Paths, PublicRosterPair, PublicRosterRead};
    use atpkg_keys::provision as prov;
    for (source_changed, floor_changed, pair_changed) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = |name| scratch.path().join(name).to_str().unwrap().to_owned();
        let paths = prov::Paths {
            pins: path("pins.rs"),
            roster: path("roster.toml"),
            key: path("machine.key"),
            machine_pub: path("machine.toml"),
            pins_explicit: true,
        };
        let pins = b"pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n";
        std::fs::write(&paths.pins, pins).unwrap();
        let pre = prov::preflight(prov::Verb::Setup, "m3", &paths).unwrap();
        let planned = prov::plan(pre, &seed(), 1_793_000_000).unwrap();
        let home = Paths::for_home(scratch.path());
        let canonical = scratch.path().join("canonical/roster.toml");
        let (master, raw, sig) = signed_candidate(3);
        let observed = admit_candidate(&[&master], raw.clone(), sig.clone()).unwrap();
        promote_public_pair(&canonical, &observed, &[&master]).unwrap();
        let (_, next_raw, next_sig) = signed_candidate(5);
        if pair_changed {
            let next = admit_candidate(&[&master], next_raw.clone(), next_sig.clone()).unwrap();
            promote_public_pair(&canonical, &next, &[&master]).unwrap();
        }
        if floor_changed {
            aterm_uds::private_file::ensure_private_dir(&home.dir).unwrap();
            aterm_uds::private_file::write_private(&home.dir, "release-ring-roster-floor", b"5\n")
                .unwrap();
        }
        let result = publish_mint_plan(planned, || {
            let (roster, signature) = if source_changed {
                (next_raw, next_sig)
            } else {
                (raw, sig)
            };
            require_public_source(&home, Some(&observed), 0, &[&master], |_| {
                Ok(PublicRosterRead::Pair(PublicRosterPair {
                    roster,
                    signature,
                }))
            })?;
            require_public_pair(&canonical, &observed, &[&master])
        });
        if !source_changed && !floor_changed && !pair_changed {
            let (_, expected) = result.unwrap();
            assert_eq!(std::fs::read(&paths.roster).unwrap(), expected.raw);
            assert_eq!(
                std::fs::read(machines::RosterDocument::signature_path(Path::new(
                    &paths.roster
                )))
                .unwrap(),
                expected.sig
            );
            assert!(Path::new(&paths.key).is_file());
        } else {
            assert!(result.is_err());
            assert_eq!(std::fs::read(&paths.pins).unwrap(), pins);
            for file in [&paths.roster, &paths.key, &paths.machine_pub] {
                assert!(!Path::new(file).exists());
            }
            assert!(!machines::RosterDocument::signature_path(Path::new(&paths.roster)).exists());
        }
    }
}

#[test]
fn newer_public_denials_survive_failed_authorization_and_dist_cleanup() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dist = scratch.path().join("dist/aterm-machines.toml");
    let kept = scratch.path().join("kept/aterm-machines.toml");
    let (master, bytes, sig) = signed_candidate(3);
    let old = admit_candidate(&[&master], bytes, sig).unwrap();
    promote_public_pair(&kept, &old, &[&master]).unwrap();
    promote_public_pair(&dist, &old, &[&master]).unwrap();
    let bytes = roster_with(5, vec![machine("m3", 0x42)], vec!["m3".into()])
        .to_toml()
        .unwrap()
        .into_bytes();
    let sig = seed().sign(&bytes).unwrap();
    let denied = admit_candidate(&[&master], bytes.clone(), sig).unwrap();
    promote_public_pair(&kept, &denied, &[&master]).unwrap();
    promote_public_pair(&dist, &denied, &[&master]).unwrap();
    assert!(
        machines::authorize_cut(
            &[&master],
            denied.bytes.clone(),
            &denied.sig,
            &pubkey_of(0x42),
            100
        )
        .is_err()
    );
    std::fs::remove_file(&dist).unwrap();
    std::fs::remove_file(machines::RosterDocument::signature_path(&dist)).unwrap();
    require_public_pair(&kept, &denied, &[&master]).unwrap();
    assert!(promote_public_pair(&kept, &old, &[&master]).is_err());
    assert!(require_public_pair(&kept, &old, &[&master]).is_err());
    let (_, fork, sig) = signed_candidate(5);
    let fork = admit_candidate(&[&master], fork, sig).unwrap();
    assert!(promote_public_pair(&kept, &fork, &[&master]).is_err());
    let retained = read_local_candidate_with(&kept, &[&master])
        .unwrap()
        .unwrap();
    assert_eq!(retained.bytes, bytes);
    assert!(
        machines::authorize_cut(
            &[&master],
            retained.bytes,
            &retained.sig,
            &pubkey_of(0x42),
            100
        )
        .is_err()
    );
}

/// The selected profile's valid pair is considered alongside the other local copies.
/// Newer removal, a fork, a damaged signature and a half-pair all refuse admission.
#[test]
fn profile_roster_participates_without_hiding_newer_or_invalid_local_state() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dist = scratch.path().join("dist/aterm-machines.toml");
    let kept = scratch.path().join("kept/aterm-machines.toml");
    let named = scratch.path().join("profile/aterm-machines.toml");
    let master = seed().pubkey_b64().unwrap();
    let write = |path: &Path, seq, members, revoked| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = roster_with(seq, members, revoked)
            .to_toml()
            .unwrap()
            .into_bytes();
        let sig = seed().sign(&bytes).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::write(machines::RosterDocument::signature_path(path), sig).unwrap();
    };
    let choose = || newest_fixture_candidate(&[&dist, &kept, &named], &[&master]);
    let authorize = |candidate: Candidate| {
        machines::authorize_cut(
            &[&master],
            candidate.bytes,
            &candidate.sig,
            &pubkey_of(0x23),
            1_793_000_000,
        )
    };

    write(&dist, 3, vec![machine("m3", 0x42)], vec![]);
    write(&named, 5, vec![machine("m23", 0x23)], vec![]);
    let chosen = choose().unwrap().unwrap();
    assert_eq!(chosen.roster.roster_seq, 5);
    assert_eq!(authorize(chosen).unwrap().machine_id, "m23");

    write(&kept, 7, vec![machine("m3", 0x42)], vec!["m23".into()]);
    let chosen = choose().unwrap().unwrap();
    assert_eq!(chosen.roster.roster_seq, 7);
    assert!(
        authorize(chosen).is_err(),
        "the older profile cannot restore membership"
    );

    write(&named, 7, vec![machine("m23", 0x23)], vec![]);
    assert!(
        choose().is_err(),
        "different signed documents at one generation refuse"
    );

    write(&named, 5, vec![machine("m23", 0x23)], vec![]);
    let signature = machines::RosterDocument::signature_path(&named);
    let mut damaged = std::fs::read(&signature).unwrap();
    damaged[0] ^= 1;
    std::fs::write(&signature, damaged).unwrap();
    assert!(choose().is_err(), "a bad named signature is never ignored");
    std::fs::remove_file(&signature).unwrap();
    assert!(choose().is_err(), "a half named pair is never ignored");
}

#[test]
fn admission_requires_the_master_signature() {
    let (master, bytes, sig) = signed_candidate(7);
    let ok = admit_candidate(&[&master], bytes.clone(), sig.clone()).expect("admits");
    assert_eq!(ok.roster.roster_seq, 7);

    let mut tampered = bytes.clone();
    tampered[0] ^= 1;
    assert!(admit_candidate(&[&master], tampered, sig.clone()).is_err());

    let other = pubkey_of(0x99);
    assert!(admit_candidate(&[&other], bytes, sig).is_err());
}

#[test]
fn the_newest_generation_wins_and_never_downgrades() {
    let master = seed().pubkey_b64().unwrap();
    let candidate = |seq| {
        let (_, bytes, sig) = signed_candidate(seq);
        admit_candidate(&[&master], bytes, sig).unwrap()
    };
    for generations in [vec![3, 5], vec![9, 3], vec![4, 4], vec![2]] {
        let expected = *generations.iter().max().unwrap();
        let selected =
            machines::current_roster::select_all(generations.into_iter().map(candidate).collect())
                .unwrap()
                .unwrap();
        assert_eq!(selected.roster.roster_seq, expected);
    }
    assert!(
        machines::current_roster::select_all(Vec::new())
            .unwrap()
            .is_none()
    );
}

#[test]
fn equal_generation_with_different_bytes_is_a_lineage_fork_and_a_hard_stop() {
    let (master, bytes, sig) = signed_candidate_with(4, vec![machine("m3", 0x42)]);
    let local = admit_candidate(&[&master], bytes, sig).unwrap();
    let (_, bytes, sig) = signed_candidate_with(4, vec![machine("m3", 0x42), machine("mx", 0x43)]);
    let other = admit_candidate(&[&master], bytes, sig).unwrap();
    assert!(machines::current_roster::select_all(vec![local, other]).is_err());
}

#[test]
fn local_roster_state_that_cannot_be_proven_is_a_hard_stop_not_a_reseed() {
    let scratch = crate::test_scratch_dir("local");
    let path = scratch.path().join("aterm-machines.toml");

    std::fs::write(&path, b"schema = 1\n").unwrap();
    assert!(read_local_candidate_with(&path, pins::PAPER_MASTER_PUBKEYS).is_err());

    std::fs::remove_file(&path).unwrap();
    std::fs::write(machines::RosterDocument::signature_path(&path), [0u8; 64]).unwrap();
    assert!(read_local_candidate_with(&path, pins::PAPER_MASTER_PUBKEYS).is_err());

    std::fs::write(&path, b"schema = 1\n").unwrap();
    assert!(read_local_candidate_with(&path, pins::PAPER_MASTER_PUBKEYS).is_err());
}

#[test]
fn devid_parsing_filters_by_kind_and_team() {
    let listing = concat!(
        "  1) 1111111111111111111111111111111111111111 \"Developer ID Application: A (TEAMIDXXXX)\"\n",
        "  2) 2222222222222222222222222222222222222222 \"Apple Development: A (TEAMIDXXXX)\"\n",
        "  3) 3333333333333333333333333333333333333333 \"Developer ID Application: A (OTHERTEAMX)\"\n",
        "  4) 4444444444444444444444444444444444444444 \"Developer ID Application: B (TEAMIDXXXX)\"\n",
        "     4 valid identities found\n",
    );
    assert_eq!(
        crate::release_keychain::devid_identities(listing, "TEAMIDXXXX"),
        vec![
            "1111111111111111111111111111111111111111".to_string(),
            "4444444444444444444444444444444444444444".to_string(),
        ]
    );
    assert!(crate::release_keychain::devid_identities("", "TEAMIDXXXX").is_empty());
}

/// A fresh Ed25519 keypair as base64 PKCS#8 (the shape `signing_key` carries) and the
/// public key it derives.
#[cfg(target_os = "macos")]
fn signing_key_b64() -> (String, String) {
    use ring::signature::KeyPair as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keypair");
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(doc.as_ref()).expect("keypair");

    (
        aterm_codec::base64::encode(doc.as_ref()).expect("pkcs8"),
        aterm_codec::base64::encode(pair.public_key().as_ref()).expect("32-byte key"),
    )
}

/// An alternate profile is audited in place; neither an absent nor an invalid default
/// changes which credentials the cut selected. Its own permissions still apply.
#[cfg(target_os = "macos")]
#[test]
fn an_explicit_profile_is_audited_without_replacing_the_default() {
    use crate::release_keychain::Leave;
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("explicit");
    let home = scratch.path();
    let home_s = home.to_str().unwrap();
    let explicit = home.join("selected.toml");
    let default = home.join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL);
    let (key, pubkey) = signing_key_b64();
    let sha1 = "0".repeat(40);
    let good = format!(
        "signing_key = \"{key}\"\nmachine_id = \"m9\"\nnotary_profile = \"notary\"\n\
         signing_identity_sha1 = \"{sha1}\"\n"
    );
    std::fs::write(&explicit, &good).unwrap();
    std::fs::set_permissions(&explicit, std::fs::Permissions::from_mode(0o600)).unwrap();
    let fake = crate::release_keychain::fake::Fake::new(crate::release_keychain::fake::World::new(
        &"F".repeat(40),
    ));
    let check = || {
        profile_check_at(
            &explicit,
            home_s,
            "m9",
            Some(&sha1),
            Some(&pubkey),
            &fake,
            Leave::Look,
        )
    };
    assert!(!default.exists());
    assert!(matches!(check(), Check::Pass(_)));
    assert!(
        !default.exists(),
        "the audit does not materialize a default"
    );

    std::fs::create_dir_all(default.parent().unwrap()).unwrap();
    std::fs::write(&default, "invalid default").unwrap();
    assert!(matches!(check(), Check::Pass(_)));
    assert_eq!(std::fs::read(&explicit).unwrap(), good.as_bytes());
    assert_eq!(std::fs::read(&default).unwrap(), b"invalid default");

    std::fs::write(&default, &good).unwrap();
    std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&explicit, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(check(), Check::Fail { .. }));
    assert_eq!(std::fs::read(&explicit).unwrap(), good.as_bytes());
}

/// The audit refuses exactly what the cut refuses: each profile below is one the cut
/// rejects on its first line.
#[cfg(target_os = "macos")]
#[test]
fn the_profile_audit_refuses_what_the_cut_refuses() {
    use crate::release_keychain::Leave;
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("profile");
    let home = scratch.path();
    std::fs::create_dir_all(home.join(atpkg_keys::provision::RELEASE_RING_REL)).unwrap();
    let path = home.join(atpkg_keys::provision::RELEASE_CREDENTIALS_REL);
    let home_s = home.to_str().unwrap().to_string();
    let sha1 = "0".repeat(40);

    let write = |body: &str, mode: u32| {
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let refusal = |c: Check| match c {
        Check::Fail { .. } => {}
        Check::Pass(msg) => panic!("expected a refusal, got PASS: {msg}"),
        _ => panic!("expected a refusal"),
    };

    let (key, pubkey) = signing_key_b64();
    let mine = Some(pubkey.as_str());
    // Keychains modelled in the scratch HOME, holding no identity: no real one is read.
    let fake = crate::release_keychain::fake::Fake::new(crate::release_keychain::fake::World::new(
        &"F".repeat(40),
    ));

    refusal(profile_check(
        &home_s,
        "m9",
        Some(&sha1),
        mine,
        &fake,
        Leave::Look,
    ));

    write(
        &format!("notary_profile = \"notary\"\nsigning_identity_sha1 = \"{sha1}\"\n"),
        0o600,
    );
    refusal(profile_check(
        &home_s,
        "m9",
        Some(&sha1),
        mine,
        &fake,
        Leave::Look,
    ));

    let good = format!(
        "signing_key = \"{key}\"\nmachine_id = \"m9\"\nnotary_profile = \"notary\"\n\
         signing_identity_sha1 = \"{sha1}\"\n"
    );
    write(&good, 0o644);
    refusal(profile_check(
        &home_s,
        "m9",
        Some(&sha1),
        mine,
        &fake,
        Leave::Look,
    ));

    write(&good, 0o600);
    refusal(profile_check(
        &home_s,
        "m9",
        Some(&"1".repeat(40)),
        mine,
        &fake,
        Leave::Look,
    ));
    assert!(
        !fake.calls("security", "find-identity").is_empty(),
        "the pinned identity is looked up in the modelled keychains"
    );

    refusal(profile_check(
        &home_s,
        "m8",
        Some(&sha1),
        mine,
        &fake,
        Leave::Look,
    ));

    let (_, other) = signing_key_b64();
    refusal(profile_check(
        &home_s,
        "m9",
        Some(&sha1),
        Some(&other),
        &fake,
        Leave::Look,
    ));

    write(
        &format!(
            "{good}machine_roster = \"{}\"\n",
            home.join("gone/roster.toml").display()
        ),
        0o600,
    );
    refusal(profile_check(
        &home_s,
        "m9",
        Some(&sha1),
        mine,
        &fake,
        Leave::Look,
    ));

    write(
        &format!("{good}machine_roster = \"~/fleet/roster.toml\"\n"),
        0o600,
    );
    refusal(profile_check(
        &home_s,
        "m9",
        Some(&sha1),
        mine,
        &fake,
        Leave::Look,
    ));
    std::fs::create_dir_all(home.join("fleet")).unwrap();
    std::fs::write(home.join("fleet/roster.toml"), "").unwrap();
    assert!(matches!(
        profile_check(&home_s, "m9", Some(&sha1), mine, &fake, Leave::Look),
        Check::Pass(_)
    ));

    write(&good, 0o600);
    assert!(matches!(
        profile_check(&home_s, "m9", Some(&sha1), mine, &fake, Leave::Look),
        Check::Pass(_)
    ));
    write(
        &format!("signing_key = \"{key}\"\nnotary_profile = \"notary\"\n"),
        0o600,
    );
    assert!(matches!(
        profile_check(&home_s, "m9", Some(&sha1), mine, &fake, Leave::Look),
        Check::Pass(_)
    ));
}

#[test]
fn waiting_on_apple_is_counted_apart_from_a_fault() {
    let fail = || Check::Fail {
        what: String::new(),
        fix: String::new(),
    };
    let todo = || Check::Todo {
        what: String::new(),
        next: String::new(),
    };
    let checks = vec![
        ("a", fail()),
        ("b", todo()),
        ("c", Check::Pass(String::new())),
    ];
    assert_eq!(tally(&checks), (1, 1));
    assert_eq!(blockers(&checks), 2);
}

#[test]
fn the_authorized_generation_is_kept_outside_the_sweepable_checkout() {
    let scratch = crate::test_scratch_dir("keep");
    let dir = scratch.path();
    let dist = dir.join("dist").join("aterm-machines.toml");
    let home = dir.join("home");
    let kept = kept_roster_path(home.to_str().unwrap());
    let lock = lock_roster_pair(&dist).unwrap();

    let (master, bytes, sig) = signed_candidate(6);
    assert!(
        newest_fixture_candidate(&[&dist, &kept], &[&master])
            .unwrap()
            .is_none()
    );
    let c = admit_candidate(&[&master], bytes, sig).unwrap();
    write_pair(&lock, &dist, &c.bytes, &c.sig).unwrap();
    drop(lock);

    assert!(keep_roster_pair(&dist, &kept, &[&master]).unwrap());
    let (a, b) = (
        machines::RosterDocument::read(&dist).unwrap(),
        machines::RosterDocument::read(&kept).unwrap(),
    );
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(a.signature, b.signature);
    assert!(!keep_roster_pair(&dist, &kept, &[&master]).unwrap());

    require_public_pair(&dist, &c, &[&master]).unwrap();

    let kept_sig = machines::RosterDocument::signature_path(&kept);
    let sig_bytes = std::fs::read(&kept_sig).unwrap();
    std::fs::remove_file(&kept_sig).unwrap();
    keep_roster_pair(&dist, &kept, &[&master]).unwrap_err();
    assert!(!kept_sig.exists(), "a refused copy wrote nothing");
    std::fs::write(&kept_sig, &sig_bytes).unwrap();
    assert!(
        !keep_roster_pair(&dist, &kept, &[&master]).unwrap(),
        "and it is whole again"
    );

    std::fs::remove_file(&dist).unwrap();
    std::fs::remove_file(machines::RosterDocument::signature_path(&dist)).unwrap();
    let selected = newest_fixture_candidate(&[&dist, &kept], &[&master])
        .unwrap()
        .unwrap();
    promote_public_pair(&dist, &selected, &[&master]).unwrap();
    require_public_pair(&dist, &c, &[&master]).unwrap();

    std::fs::remove_file(machines::RosterDocument::signature_path(&kept)).unwrap();
    assert!(newest_fixture_candidate(&[&dist, &kept], &[&master]).is_err());
}

#[test]
fn the_front_door_is_targo_and_trustc_answering_version_with_no_rustup() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("front");
    let bin = scratch.path().join("store/trust/9192/bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["targo", "trustc"] {
        std::fs::write(bin.join(tool), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(bin.join(tool), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let answers = |tool: &Path| -> std::result::Result<String, String> {
        Ok(format!(
            "{} 1.99.0-dev (321aaeda7 2026-09-17)\nrelease: 1.99.0-dev",
            tool.file_name().unwrap().to_string_lossy()
        ))
    };

    match front_door_verdict(&bin, answers) {
        Check::Pass(_) => {}
        other => panic!("a store bin whose pair answers is the golden path: {other:?}"),
    }

    let mute = |tool: &Path| -> std::result::Result<String, String> {
        if tool.ends_with("trustc") {
            Err("dyld: Library not loaded".into())
        } else {
            answers(tool)
        }
    };
    match front_door_verdict(&bin, mute) {
        Check::Fail { .. } => {}
        other => panic!("a mute compiler is a GAP: {other:?}"),
    }

    std::fs::remove_file(bin.join("targo")).unwrap();
    match front_door_verdict(&bin, answers) {
        Check::Fail { .. } => {}
        other => panic!("a missing driver is a GAP: {other:?}"),
    }
}

#[test]
fn a_stage2_missing_its_verifiers_is_rebuilt_in_a_trust_checkout() {
    let scratch = crate::test_scratch_dir("verifier");
    let bin = scratch.path();
    match verifiers_check(bin) {
        Check::Fail { what, fix } => {
            assert!(
                what.starts_with("targo + tippy + ty + trustdoc missing"),
                "{what}"
            );
            assert!(!fix.contains('~'), "a home folder: {fix}");
            assert!(fix.contains("the Trust checkout's bootstrap.toml"), "{fix}");
            assert!(fix.contains(gates::TRUST_FROM_SOURCE), "{fix}");
        }
        other => panic!("an empty bin is missing every verifier: {other:?}"),
    }
}

#[test]
fn path_resolution_finds_only_a_real_executable_and_in_path_order() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("path");
    let dir = scratch.path();
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let joined = std::env::join_paths([&a, &b]).unwrap();

    assert_eq!(resolve_on_path("trustdoc", &joined), None);

    std::fs::write(b.join("trustdoc"), b"").unwrap();
    assert_eq!(resolve_on_path("trustdoc", &joined), None);

    std::fs::set_permissions(b.join("trustdoc"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        resolve_on_path("trustdoc", &joined),
        Some(b.join("trustdoc"))
    );

    std::fs::write(a.join("trustdoc"), b"").unwrap();
    std::fs::set_permissions(a.join("trustdoc"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        resolve_on_path("trustdoc", &joined),
        Some(a.join("trustdoc"))
    );
}

#[test]
fn the_farm_link_is_created_repairs_a_dangling_link_and_replaces_nothing_else() {
    let scratch = crate::test_scratch_dir("farm");
    let dir = scratch.path();
    let target = dir.join("stage2-trustdoc");
    std::fs::write(&target, b"#!/bin/sh\n").unwrap();
    let farm = dir.join("farm/bin/trustdoc");

    link_farm(&farm, &target).expect("create");
    assert_eq!(std::fs::read_link(&farm).unwrap(), target);
    let staged = {
        let mut s = farm.as_os_str().to_owned();
        s.push(".provision.tmp");
        std::path::PathBuf::from(s)
    };
    assert!(
        std::fs::symlink_metadata(&staged).is_err(),
        "the staged name never survives a promote"
    );

    std::fs::remove_file(&farm).unwrap();
    std::os::unix::fs::symlink(dir.join("gone"), &farm).unwrap();
    link_farm(&farm, &target).expect("repair dangling");
    assert_eq!(std::fs::read_link(&farm).unwrap(), target);

    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::remove_file(&farm).unwrap();
        let sealed = dir.join("sealed");
        std::fs::create_dir_all(&sealed).unwrap();
        std::fs::write(sealed.join("trustdoc"), b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(sealed.join("trustdoc"), &farm).unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let refused = link_farm(&farm, &target);
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
        refused.unwrap_err();
    }

    std::fs::remove_file(&farm).unwrap();
    std::os::unix::fs::symlink(&target, &farm).unwrap();
    let other = dir.join("other-trustdoc");
    std::fs::write(&other, b"#!/bin/sh\n").unwrap();
    link_farm(&farm, &other).unwrap_err();
    assert_eq!(std::fs::read_link(&farm).unwrap(), target);

    std::fs::remove_file(&farm).unwrap();
    std::fs::write(&farm, b"not a link").unwrap();
    link_farm(&farm, &target).unwrap_err();
    assert_eq!(std::fs::read_to_string(&farm).unwrap(), "not a link");
}

#[test]
fn a_death_between_the_phase_one_pair_renames_recovers_forward() {
    let scratch = crate::test_scratch_dir("phase1");
    let dist = scratch.path().join("dist").join("aterm-machines.toml");
    let sig_path = machines::RosterDocument::signature_path(&dist);

    let (master, six, six_sig) = signed_candidate(6);
    let chosen = admit_candidate(&[&master], six.clone(), six_sig.clone()).unwrap();
    let lock = lock_roster_pair(&dist).expect("the phase's writer lock");
    write_pair(&lock, &dist, &chosen.bytes, &chosen.sig).expect("phase 1 installs the chosen pair");
    assert_eq!(std::fs::read(&dist).unwrap(), six);
    assert_eq!(std::fs::read(&sig_path).unwrap(), six_sig);

    let (_, seven, seven_sig) = signed_candidate(7);
    let premise = atpkg_keys::provision::RosterSnapshot {
        raw: six.clone(),
        sig: six_sig.clone(),
    };
    let committed = atpkg_keys::provision::commit_roster_pair(
        &lock,
        dist.to_str().unwrap(),
        Some(&premise),
        &seven,
        &seven_sig,
    )
    .expect("the redo transaction commits");
    let transaction = committed.transaction_path().to_string();
    drop(committed);
    std::fs::write(&dist, &seven).unwrap();
    drop(lock);

    let torn = machines::RosterDocument::read(&dist).expect("both halves are present");
    assert_eq!(torn.bytes, seven);
    assert_eq!(torn.signature, six_sig);
    assert!(
        admit_candidate(&[&master], torn.bytes, torn.signature).is_err(),
        "a torn pair is exactly what no client — and no operator — can make sense of"
    );
    assert!(
        std::path::Path::new(&transaction).exists(),
        "the redo log committed before either rename, so it survived the death"
    );

    let lock = lock_roster_pair(&dist).expect("the phase-1 lock replays the committed redo");
    assert_eq!(std::fs::read(&dist).unwrap(), seven);
    assert_eq!(std::fs::read(&sig_path).unwrap(), seven_sig);
    let recovered = machines::RosterDocument::read(&dist).expect("a whole pair");
    assert_eq!(
        admit_candidate(&[&master], recovered.bytes, recovered.signature)
            .expect("the recovered pair verifies")
            .roster
            .roster_seq,
        7,
        "forward, not sideways: the generation the dead run signed"
    );
    assert!(
        !std::path::Path::new(&transaction).exists(),
        "a completed transaction is retired, so it cannot replay a second time"
    );

    let (_, eight, eight_sig) = signed_candidate(8);
    write_pair(&lock, &dist, &eight, &eight_sig).expect("phase 1 writes on");
    assert_eq!(std::fs::read(&dist).unwrap(), eight);
    assert_eq!(std::fs::read(&sig_path).unwrap(), eight_sig);
    assert!(!std::path::Path::new(&transaction).exists());
    drop(lock);
}
