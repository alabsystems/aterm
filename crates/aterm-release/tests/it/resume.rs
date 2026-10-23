// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure-logic resume, recut, publish helper, and CLI parse tests.

use crate::{cli, ledger, manifest_out, publish, verify};

use std::path::PathBuf;
use std::sync::Mutex;

use publish::{Journal, STEPS};
use verify::{CutMode, RemoteState};

/// A fresh directory under `<target tmp>/resume`, removed on drop.
fn tmpdir(name: &str) -> aterm_tempfile::TempDir {
    let parent = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("resume");
    std::fs::create_dir_all(&parent).expect("create tmpdir");
    aterm_tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir_in(parent)
        .expect("create tmpdir")
}

fn state(current_version: &str, has_section: bool, published: bool) -> RemoteState {
    RemoteState {
        current_version: current_version.to_string(),
        changelog_has_section: has_section,
        published,
    }
}

#[test]
fn remote_derived_cut_mode_decision_table() {
    let fresh = |v: &str| CutMode::Fresh {
        version: v.to_string(),
    };
    let recut = |v: &str| CutMode::Recut {
        version: v.to_string(),
    };

    let table: &[(&str, bool, bool, CutMode)] = &[
        ("0.3.0", false, false, fresh("0.3.0")),
        ("0.2.0", true, false, recut("0.2.0")),
        ("0.9.0", false, false, fresh("0.9.0")),
        ("1.99.0", false, false, fresh("1.99.0")),
    ];
    for (short, section, published, want) in table {
        let got = verify::derive_cut_mode(&state(short, *section, *published))
            .unwrap_or_else(|e| panic!("({short}, {section}, {published}) errored: {e}"));
        assert_eq!(&got, want, "({short}, {section}, {published})");
    }
}

#[test]
fn recutting_a_published_version_is_refused() {
    assert!(verify::derive_cut_mode(&state("0.2.0", true, true)).is_err());
    assert!(verify::derive_cut_mode(&state("0.2.0", false, true)).is_err());
}

fn journal() -> Journal {
    Journal {
        verify_pubkey: None,
        format: publish::JOURNAL_FORMAT,
        version: "0.26.0".into(),
        build_number: 1_783_918_101,
        commit: "aed5a06caed5a06caed5a06caed5a06caed5a06c".into(),
        min_build: None,
        artifact: Some("aa".repeat(32)),
        macho_sha256: Some("bb".repeat(32)),
        linux: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        done: vec![],
    }
}

#[test]
fn first_incomplete_walks_the_step_order() {
    let mut j = journal();
    assert_eq!(
        j.first_incomplete(),
        Some("lock"),
        "a fresh journal acquires the remote lease before build"
    );
    j.done = vec!["lock".into(), "sign".into(), "selfcheck".into()];
    assert_eq!(j.first_incomplete(), Some("tag"));
    j.done = ["lock", "sign", "selfcheck", "tag"]
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        j.first_incomplete(),
        Some("publish"),
        "a crash anywhere in the one publication resumes into its convergence"
    );
    j.done = vec![
        "selfcheck".into(),
        "lock".into(),
        "sign".into(),
        "tag".into(),
    ];
    assert_eq!(j.first_incomplete(), Some("publish"));
    j.done = vec!["lock".into(), "sign".into(), "tag".into()];
    assert_eq!(j.first_incomplete(), Some("selfcheck"));
    j.done = STEPS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        j.first_incomplete(),
        None,
        "a completed cut has nothing to resume"
    );
}

#[test]
fn current_journal_done_state_is_an_exact_canonical_prefix() {
    let dir = tmpdir("journal-prefix");
    let path = dir.path().join("state.toml");
    for done in [
        vec!["sign".to_string()],
        vec!["lock".to_string(), "publish".to_string()],
        vec!["lock".to_string(), "lock".to_string()],
        vec!["lock".to_string(), "unknown".to_string()],
    ] {
        let mut corrupted = journal();
        corrupted.done = done;
        assert!(corrupted.save(&path).is_err());
    }
    let mut valid = journal();
    valid.done = STEPS[..5].iter().map(|step| (*step).to_string()).collect();
    valid.release_id = Some(55);
    valid.release_intent = true;
    valid.upload_intents = vec!["aterm-0.26.0.dmg".into()];
    valid.save(&path).unwrap();
    assert_eq!(Journal::load(&path).unwrap().unwrap(), valid);

    for malformed in ["0.26", "v0.26.0", "0.26.0.1", "01.26.0"] {
        let mut malformed_identity = journal();
        malformed_identity.version = malformed.into();
        assert!(
            malformed_identity.save(&path).is_err(),
            "{malformed:?} must not reach disk"
        );
    }
    let mut malformed_owner = journal();
    malformed_owner.commit = "abcd".into();
    assert!(malformed_owner.save(&path).is_err());
}

#[test]
fn an_older_journal_format_is_refused_and_triaged_by_its_header() {
    let dir = tmpdir("journal-older-format");
    let path = dir.path().join("cut-state.toml");
    let through_unlock = "\"lock\", \"build\", \"selfcheck\", \"draft\", \"upload\", \
         \"preflip\", \"tag\", \"flip\", \"archive\", \"verify\", \"mirror\", \"unlock\"";
    let write = |format: &str, done: &str| {
        std::fs::write(
            &path,
            format!(
                "{format}version = \"0.91.0\"\nbuild_number = 1790120000\n\
                 commit = \"{}\"\nrelease_id = 55\nrelease_intent = true\n\
                 done = [{done}]\n",
                "f".repeat(40)
            ),
        )
        .unwrap();
    };
    let publish_once = "\"lock\", \"build\", \"selfcheck\", \"tag\", \"publish\", \"unlock\"";
    for (format, done, named) in [
        ("format = 11\n", publish_once.to_string(), "format-11"),
        ("format = 10\n", through_unlock.to_string(), "format-10"),
        (
            "format = 9\n",
            format!("{through_unlock}, \"site\""),
            "format-9",
        ),
        ("format = 9\n", through_unlock.to_string(), "format-9"),
        (
            "format = 8\n",
            format!("{through_unlock}, \"site\""),
            "format-8",
        ),
        ("format = 8\n", through_unlock.to_string(), "format-8"),
        ("format = 7\n", through_unlock.to_string(), "format-7"),
        ("", "\"build\", \"selfcheck\"".to_string(), "format-1"),
    ] {
        write(format, &done);
        let error = Journal::load(&path)
            .expect_err("an older journal is refused, never walked")
            .to_string();
        assert!(error.contains(named), "{error}");
        assert!(
            Journal::load_if_ours(&path).unwrap().is_none(),
            "a read-only command takes nothing from it, and does not fail on it"
        );
        let header = publish::JournalHeader::read(&path).unwrap().unwrap();
        let triage = publish::fresh_cut_journal_triage(Some(&header), publish::CutKind::Real);
        if done.contains("\"unlock\"") {
            assert!(triage.unwrap(), "{named}: a finished journal is history");
        } else {
            triage.expect_err("an unfinished one blocks");
        }
    }
    let now_through_unlock = "\"lock\", \"sign\", \"selfcheck\", \"tag\", \"publish\", \"unlock\"";
    write(
        &format!("format = {}\n", publish::JOURNAL_FORMAT),
        now_through_unlock,
    );
    let finished = Journal::load(&path).unwrap().unwrap();
    assert_eq!(finished.first_incomplete(), None);
    assert!(
        publish::fresh_cut_journal_triage(
            Some(&publish::JournalHeader::read(&path).unwrap().unwrap()),
            publish::CutKind::Real
        )
        .unwrap()
    );
    write(
        &format!("format = {}\n", publish::JOURNAL_FORMAT),
        "\"lock\", \"sign\", \"selfcheck\", \"tag\"",
    );
    let in_flight = Journal::load(&path).unwrap().unwrap();
    assert_eq!(in_flight.first_incomplete(), Some("publish"));
    publish::fresh_cut_journal_triage(
        Some(&publish::JournalHeader::read(&path).unwrap().unwrap()),
        publish::CutKind::Real,
    )
    .unwrap_err();
    for stale in [
        through_unlock.to_string(),
        publish_once.to_string(),
        format!("{now_through_unlock}, \"site\""),
    ] {
        write(&format!("format = {}\n", publish::JOURNAL_FORMAT), &stale);
        assert!(Journal::load(&path).is_err(), "{stale}");
    }
    assert!(!publish::fresh_cut_journal_triage(None, publish::CutKind::Real).unwrap());
}

#[test]
fn the_finished_0_92_journal_main_cut_is_refused_by_name() {
    const V0_92_JOURNAL: &str = r#"format = 9
version = "0.92.0"
build_number = 1790278596
commit = "e8a8c80ab93cdf5bdfd048e22514b663874bb57a"
min_build = 1786131079
arm64_only = false
manifest_signed = true
signature_required = true
signature_pubkey = "YOHw0OoefQ79NdE8qsQFobIMR7QXChpreYBi2Of74Uo="
signature_machine_id = "m3"
release_id = 396027000
draft_create_issued = true
upload_intents = ["aterm-0.92.0.dmg", "aterm-0.92.0.dmg.sha256", "aterm-0.92.0-mac.zip", "aterm-0.92.0-mac.zip.sha256", "aterm-appcast.toml", "aterm-0.92.0-build.txt", "aterm-machines.toml", "aterm-machines.toml.sig", "aterm-appcast.toml.sig", "aterm-0.92.0-dSYM.zip"]
mirror_release_id = 396014794
mirror_create_issued = true
mirror_upload_intents = ["aterm-0.92.0-mac.zip", "aterm-0.92.0-mac.zip.sha256", "aterm-0.92.0.dmg", "aterm-0.92.0.dmg.sha256", "aterm-mac.zip", "aterm-mac.zip.sha256", "aterm-machines.toml", "aterm-machines.toml.sig", "aterm.dmg", "aterm.dmg.sha256", "aterm-appcast.toml", "aterm-appcast.toml.sig"]
done = ["lock", "build", "selfcheck", "draft", "upload", "preflip", "tag", "flip", "archive", "verify", "mirror", "unlock", "site"]
"#;
    assert_ne!(
        publish::JOURNAL_FORMAT,
        9,
        "9 names two step lists; the current format must be a number no other cutter wrote"
    );
    let dir = tmpdir("journal-v0-92");
    let path = dir.path().join("cut-state.toml");
    std::fs::write(&path, V0_92_JOURNAL).unwrap();
    let error = Journal::load(&path)
        .expect_err("main's finished 0.92 journal is refused, never walked")
        .to_string();
    assert!(error.contains("format-9"), "{error}");
    let header = publish::JournalHeader::read(&path).unwrap().unwrap();
    assert!(header.finished());
    assert!(publish::fresh_cut_journal_triage(Some(&header), publish::CutKind::Real).unwrap());

    let current = V0_92_JOURNAL
        .lines()
        .filter(|line| !line.starts_with("mirror_") && !line.starts_with("done = "))
        .map(|line| match line {
            "format = 9" => format!("format = {}", publish::JOURNAL_FORMAT),
            "draft_create_issued = true" => "release_intent = true".to_string(),
            other => other.to_string(),
        })
        .chain(std::iter::once(
            "done = [\"lock\", \"sign\", \"selfcheck\", \"tag\", \"publish\", \"unlock\"]"
                .to_string(),
        ))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, current).unwrap();
    let finished = Journal::load(&path)
        .expect("the current format loads")
        .expect("present");
    assert_eq!(finished.version, "0.92.0");
    assert_eq!(finished.done.len(), publish::STEPS.len());
    assert_eq!(finished.first_incomplete(), None);
    assert!(
        publish::fresh_cut_journal_triage(
            Some(&publish::JournalHeader::read(&path).unwrap().unwrap()),
            publish::CutKind::Real
        )
        .unwrap()
    );
}

#[test]
fn the_finished_0_93_journal_is_history_to_every_command() {
    const V0_93_JOURNAL: &str = r#"format = 10
version = "0.93.0"
build_number = 1790305290
commit = "73a9b424007d54efc8aad91354dd1151623e2a40"
min_build = 1786131079
arm64_only = false
manifest_signed = true
signature_required = true
signature_pubkey = "YOHw0OoefQ79NdE8qsQFobIMR7QXChpreYBi2Of74Uo="
signature_machine_id = "m3"
release_id = 396261568
draft_create_issued = true
upload_intents = ["aterm-0.93.0.dmg", "aterm-0.93.0.dmg.sha256", "aterm-0.93.0-mac.zip", "aterm-0.93.0-mac.zip.sha256", "aterm-appcast.toml", "aterm-0.93.0-build.txt", "aterm-machines.toml", "aterm-machines.toml.sig", "aterm-appcast.toml.sig", "aterm-0.93.0-dSYM.zip"]
mirror_release_id = 396254439
mirror_create_issued = true
mirror_upload_intents = ["aterm-0.93.0-mac.zip", "aterm-0.93.0-mac.zip.sha256", "aterm-0.93.0.dmg", "aterm-0.93.0.dmg.sha256", "aterm-mac.zip", "aterm-mac.zip.sha256", "aterm.dmg", "aterm.dmg.sha256", "aterm-appcast.toml.sig", "aterm-appcast.toml"]
done = ["lock", "build", "selfcheck", "draft", "upload", "preflip", "tag", "flip", "archive", "verify", "mirror", "unlock"]
"#;
    let dir = tmpdir("journal-v0-93");
    let path = dir.path().join("cut-state.toml");
    std::fs::write(&path, V0_93_JOURNAL).unwrap();

    let header = publish::JournalHeader::read(&path).unwrap().unwrap();
    assert_eq!(
        (
            header.format,
            header.version.as_str(),
            header.build_number,
            header.finished()
        ),
        (10, "0.93.0", 1_790_305_290, true)
    );
    for kind in [publish::CutKind::Real, publish::CutKind::DryRun] {
        assert!(
            publish::fresh_cut_journal_triage(Some(&header), kind).unwrap(),
            "{kind:?} clears it"
        );
    }
    assert!(Journal::load_if_ours(&path).unwrap().is_none());
    let error = Journal::load(&path).unwrap_err().to_string();
    assert!(error.contains("format-10"), "{error}");

    std::fs::write(
        &path,
        V0_93_JOURNAL.replace(", \"mirror\", \"unlock\"]", "]"),
    )
    .unwrap();
    let header = publish::JournalHeader::read(&path).unwrap().unwrap();
    assert!(!header.finished());
    publish::fresh_cut_journal_triage(Some(&header), publish::CutKind::Real).unwrap_err();
}

#[test]
fn journal_round_trips_and_marks_persist_immediately() {
    let dir = tmpdir("journal");
    let path = dir.path().join("cut-state.toml");

    assert!(
        Journal::load(&path)
            .expect("absent journal is fine")
            .is_none()
    );

    let mut j = journal();
    j.min_build = Some(j.build_number);
    j.save(&path).expect("save");
    assert_eq!(
        Journal::load(&path).expect("load").expect("present"),
        j,
        "byte round-trip"
    );

    j.mark("lock", &path).expect("mark lock");
    j.mark("sign", &path).expect("mark sign");
    j.mark("sign", &path).expect("mark sign again");
    let back = Journal::load(&path).unwrap().unwrap();
    assert_eq!(
        back.done,
        vec!["lock".to_string(), "sign".to_string()],
        "no duplicate entries"
    );
    assert_eq!(back.first_incomplete(), Some("selfcheck"));

    std::fs::write(&path, "version = [not toml").unwrap();
    assert!(
        Journal::load(&path).is_err(),
        "corrupt journal must be a hard error"
    );
}

#[test]
fn journal_rejects_min_build_above_its_claim() {
    let dir = tmpdir("journal-invalid-floor");
    let path = dir.path().join("cut-state.toml");
    let mut j = journal();
    j.min_build = Some(j.build_number + 1);
    j.save(&path).unwrap_err();
    assert!(!path.exists(), "an invalid journal must not reach disk");

    std::fs::write(
        &path,
        format!(
            "format = {}\nversion = \"0.55.0\"\nbuild_number = 550\ncommit = \"{}\"\n\
             min_build = 551\n",
            publish::JOURNAL_FORMAT,
            "a".repeat(40)
        ),
    )
    .unwrap();
    Journal::load(&path).unwrap_err();
}

#[test]
fn journal_persists_signature_ratchet_and_actual_key_fail_closed() {
    let dir = tmpdir("journal-signature-policy");
    let path = dir.path().join("cut-state.toml");
    let mut j = journal();
    j.signature_required = true;
    j.save(&path).unwrap_err();

    j.signature_pubkey = Some("11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=".into());
    j.save(&path).unwrap();
    assert_eq!(Journal::load(&path).unwrap().unwrap(), j);

    j.done.extend(["lock".into(), "sign".into()]);
    j.save(&path).unwrap_err();
    j.manifest_signed = true;
    j.save(&path).unwrap();

    let mut downgrade = journal();
    downgrade.manifest_signed = true;
    assert!(downgrade.save(&path).is_err());
}

#[test]
fn version_helpers_port_the_shell_derivations() {
    let cargo = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.25.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n";
    assert_eq!(publish::workspace_version(cargo).unwrap(), "0.25.0");
    let decoy = "[package]\nversion = \"9.9.9\"\n\n[workspace.package]\nversion = \"0.25.0\"\n";
    assert_eq!(publish::workspace_version(decoy).unwrap(), "0.25.0");
    assert!(publish::workspace_version("[workspace]\n").is_err());

    let dev = "[workspace.package]\nversion = \"0.2.1\"\n";
    assert_eq!(publish::workspace_version(dev).unwrap(), "0.2.1");

    assert_eq!(
        publish::release_version_from_workspace("0.2.0").unwrap(),
        "0.2.0"
    );
    for bad in [
        "0.2.1", "1.10.7", "0.2", "0.2.1.1", "v0.2.1", "01.2.1", "0.2.x", "",
    ] {
        assert!(
            publish::release_version_from_workspace(bad).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn claim_provenance_binds_the_release_to_the_claims_short_commit() {
    let owner = "0123456789abcdef0123456789abcdef01234567";
    let provenance = b"version=0.59.0\nbuild=1785000000\ncommit=0123456789ab\n";

    publish::validate_claim_provenance(provenance, "0.59.0", 1_785_000_000, owner).unwrap();

    assert!(
        publish::validate_claim_provenance(
            provenance,
            "0.59.0",
            1_785_000_000,
            "fedcba9876543210fedcba9876543210fedcba98"
        )
        .is_err()
    );
    assert!(
        publish::validate_claim_provenance(
            b"version=0.59.0\nbuild=1785000000\ncommit=0123456789ab\ncommit=0123456789ab\n",
            "0.59.0",
            1_785_000_000,
            owner
        )
        .is_err()
    );
}

#[test]
fn repo_slug_parses_the_repository_field_shapes() {
    let mk =
        |url: &str| format!("[workspace.package]\nversion = \"0.25.0\"\nrepository = \"{url}\"\n");
    for url in [
        "https://github.com/alabsystems/aterm",
        "https://github.com/alabsystems/aterm.git",
        "https://github.com/alabsystems/aterm/",
        "git@github.com:alabsystems/aterm.git",
    ] {
        assert_eq!(
            publish::repo_slug(&mk(url)).as_deref(),
            Some("alabsystems/aterm"),
            "{url}"
        );
    }
    assert_eq!(
        publish::repo_slug(&mk("https://example.com/x/y")),
        None,
        "non-GitHub host"
    );
    assert_eq!(
        publish::repo_slug("[workspace.package]\n"),
        None,
        "absent field"
    );
}

#[test]
fn monotonic_gate_accepts_only_strictly_newer_or_our_own_cut() {
    let ok = |n, best| publish::monotonic_ok(n, "v0.26.0", best).is_ok();
    assert!(ok(100, None), "an empty repo cannot outrank us");
    assert!(ok(100, Some(("v0.25.0", 99))), "strictly newer wins");
    assert!(
        ok(100, Some(("v0.26.0", 100))),
        "our own half-flipped release is fine"
    );
    assert!(
        !ok(100, Some(("v0.99.0", 100))),
        "same build under another tag is a collision"
    );
    assert!(
        !ok(100, Some(("v0.99.0", 101))),
        "an older n than live must abort"
    );
}

#[test]
fn effective_min_build_is_monotonic_canonical_and_bounded_by_claim() {
    let resolve = |operator, channel| publish::effective_min_build(operator, channel, 100).unwrap();
    assert_eq!(resolve(None, None), None);
    assert_eq!(resolve(Some(0), None), None);
    assert_eq!(resolve(None, Some(0)), None);
    assert_eq!(resolve(Some(0), Some(0)), None);
    assert_eq!(resolve(Some(40), None), Some(40));
    assert_eq!(resolve(None, Some(70)), Some(70));
    assert_eq!(resolve(Some(40), Some(70)), Some(70));
    assert_eq!(resolve(Some(90), Some(70)), Some(90));
    assert_eq!(resolve(Some(100), Some(99)), Some(100));

    assert!(publish::effective_min_build(Some(101), Some(70), 100).is_err());
}

#[test]
fn origin_url_parser_accepts_only_unambiguous_exact_github_repo_forms() {
    for url in [
        "https://github.com/alabsystems/aterm.git",
        "git@github.com:alabsystems/aterm.git",
        "ssh://git@github.com/alabsystems/aterm.git",
    ] {
        assert_eq!(
            publish::github_slug_from_remote_url(url).unwrap(),
            "alabsystems/aterm"
        );
    }
    for bad in [
        "https://evil.example/alabsystems/aterm.git",
        "https://github.com/alabsystems/aterm/extra",
        "https://github.com/alabsystems",
        "https://github.com/alabsystems/aterm?other=true",
    ] {
        assert!(publish::github_slug_from_remote_url(bad).is_err(), "{bad}");
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty(), "odd-length hex test vector");
    pairs
        .iter()
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("non-hex test vector"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

#[test]
fn detached_signature_verifier_matches_rfc8032_and_catches_mutations() {
    let pubkey = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    let signature = decode_hex(concat!(
        "e5564300c360ac729086e2cc806e828a",
        "84877f1eb8e5d974d873e06522490155",
        "5fb8821590a33bacc61e39701cf9b46b",
        "d25bf5f0595bbe24655141438e7a100b"
    ));
    publish::verify_detached_manifest_signature(pubkey, b"", &signature).unwrap();
    assert!(publish::verify_detached_manifest_signature(pubkey, b"x", &signature).is_err());
    let mut mutated = signature.clone();
    mutated[0] ^= 1;
    assert!(publish::verify_detached_manifest_signature(pubkey, b"", &mutated).is_err());
    assert!(publish::canonical_update_pubkey("not-base64").is_err());
}

#[test]
fn release_asset_digest_replay_uses_the_updater_download_bound() {
    assert!(publish::validate_release_asset_download_size(1).is_ok());
    assert!(publish::validate_release_asset_download_size(2_147_483_648).is_ok());
    assert!(publish::validate_release_asset_download_size(0).is_err());
    assert!(publish::validate_release_asset_download_size(2_147_483_649).is_err());
    assert_eq!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_ASSET, 262_144).unwrap(),
        262_144
    );
    assert!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_ASSET, 262_145).is_err()
    );
    assert_eq!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_SIG_ASSET, 64).unwrap(),
        64
    );
    assert!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_SIG_ASSET, 63).is_err()
    );
    assert!(publish::validate_small_release_asset_size("aterm-0.55.0.dmg", 1).is_err());
    assert_eq!(
        publish::read_bounded_release_asset(std::io::Cursor::new(vec![7_u8; 64]), 64).unwrap(),
        vec![7_u8; 64]
    );
    assert!(
        publish::read_bounded_release_asset(std::io::Cursor::new(vec![7_u8; 65]), 64).is_err(),
        "a response that grows after metadata preflight stays memory-bounded"
    );
    let (diagnostic, truncated) =
        publish::drain_bounded_diagnostic(std::io::Cursor::new(vec![b'e'; 1024 * 1024]), 1024)
            .unwrap();
    assert_eq!(diagnostic.len(), 1024);
    assert!(
        truncated,
        "noisy stderr is drained but retained only to cap"
    );

    let mut exact_sink = Vec::new();
    let (size, digest) = publish::copy_bounded_release_asset(
        std::io::Cursor::new(b"exact bytes"),
        &mut exact_sink,
        11,
    )
    .unwrap();
    assert_eq!(size, 11);
    assert_eq!(exact_sink, b"exact bytes");
    assert_eq!(
        digest,
        "e38e581aade78b64cc86f7ac9f3555ca78c2dcca747942a7f1d9b3275a834f75"
    );

    let mut bounded_sink = Vec::new();
    publish::copy_bounded_release_asset(
        std::io::Cursor::new(vec![9_u8; 1025]),
        &mut bounded_sink,
        1024,
    )
    .unwrap_err();
    assert!(
        bounded_sink.len() <= 1024,
        "the over-limit probe byte must never reach disk"
    );

    assert_eq!(
        publish::parse_release_asset_identity_rows("", "v0.55.0", "asset.toml").unwrap(),
        None
    );
    assert_eq!(
        publish::parse_release_asset_identity_rows(
            "other.txt\t1\t7\nasset.toml\t42\t64\n",
            "v0.55.0",
            "asset.toml",
        )
        .unwrap(),
        Some((42, 64))
    );
    assert!(
        publish::parse_release_asset_identity_rows(
            "asset.toml\t42\t64\nasset.toml\t43\t64\n",
            "v0.55.0",
            "asset.toml",
        )
        .is_err(),
        "a duplicate exact name is ambiguous"
    );
    assert!(
        publish::parse_release_asset_identity_rows("asset.toml\t42\t0\n", "v0.55.0", "asset.toml",)
            .is_err(),
        "an empty exact-ID object is never treated as an absent optional asset"
    );
}

#[test]
fn live_signature_replay_requires_exact_unique_valid_head_without_fallback() {
    let pubkey = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    let signature = decode_hex(concat!(
        "e5564300c360ac729086e2cc806e828a",
        "84877f1eb8e5d974d873e06522490155",
        "5fb8821590a33bacc61e39701cf9b46b",
        "d25bf5f0595bbe24655141438e7a100b"
    ));
    let head = archive_release(
        "v0.55.0",
        false,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, manifest_out::MANIFEST_SIG_ASSET),
        ],
    );
    let verify = |releases: &[publish::AppcastRelease], bytes: Vec<u8>| {
        publish::verify_channel_head_signature_with(
            releases,
            "v0.55.0",
            b"",
            Some(&signature),
            pubkey,
            |_, _, tag, name| {
                assert_eq!(tag, "v0.55.0");
                assert_eq!(name, manifest_out::MANIFEST_SIG_ASSET);
                Ok(bytes.clone())
            },
        )
    };
    verify(std::slice::from_ref(&head), signature.clone()).unwrap();

    let missing = archive_release("v0.55.0", false, &[(1, manifest_out::MANIFEST_ASSET)]);
    assert!(verify(&[missing], signature.clone()).is_err());

    let duplicate = archive_release(
        "v0.55.0",
        false,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, manifest_out::MANIFEST_SIG_ASSET),
            (3, manifest_out::MANIFEST_SIG_ASSET),
        ],
    );
    assert!(verify(&[duplicate], signature.clone()).is_err());

    let archive_fallback = archive_release(
        "v0.55.0",
        false,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, "aterm-appcast-v0.55.0.toml.sig"),
        ],
    );
    assert!(verify(&[archive_fallback], signature.clone()).is_err());

    let mut corrupt = signature.clone();
    corrupt[0] ^= 1;
    let err = verify(std::slice::from_ref(&head), corrupt.clone())
        .unwrap_err()
        .to_string();
    assert!(err.contains("byte-identical"), "{err}");

    let err = publish::verify_channel_head_signature_with(
        &[head],
        "v0.55.0",
        b"",
        None,
        pubkey,
        |_, _, _, _| Ok(corrupt.clone()),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid"), "{err}");
}

#[test]
fn live_archive_identity_requires_exact_version_build_commit_and_local_bytes() {
    let commit = "a".repeat(40);
    let manifest = format!(
        "schema = 1\nversion = \"0.55.0\"\nbuild_number = 55\ncommit = \"{commit}\"\n\
         dmg = \"aterm-0.55.0.dmg\"\nsha256 = \"{}\"\n",
        "0".repeat(64)
    );
    let expected = publish::ExpectedReleaseIdentity {
        version: "0.55.0",
        build: 55,
        commit: &commit,
    };
    publish::validate_live_release_identity(
        expected,
        manifest.as_bytes(),
        None,
        Some(manifest.as_bytes()),
        None,
        false,
        None,
    )
    .unwrap();

    for bad in [
        manifest.replace("version = \"0.55.0\"", "version = \"0.56.0\""),
        manifest.replace("build_number = 55", "build_number = 56"),
        manifest.replace(&commit, &"b".repeat(40)),
        manifest.replace("aterm-0.55.0.dmg", "other.dmg"),
    ] {
        assert!(
            publish::validate_live_release_identity(
                expected,
                bad.as_bytes(),
                None,
                None,
                None,
                false,
                None,
            )
            .is_err()
        );
    }
    let err = publish::validate_live_release_identity(
        expected,
        manifest.as_bytes(),
        None,
        Some(b"different bytes"),
        None,
        false,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("byte-identical"), "{err}");
}

fn archive_release(tag: &str, draft: bool, assets: &[(u64, &str)]) -> publish::AppcastRelease {
    publish::AppcastRelease {
        release_id: assets.first().map_or(1, |(id, _)| *id),
        tag: tag.to_string(),
        draft,
        target_commitish: "a".repeat(40),
        assets: assets
            .iter()
            .map(|(id, name)| publish::AppcastAsset {
                id: *id,
                name: (*name).to_string(),
            })
            .collect(),
    }
}

/// A valid Ed25519 key (the RFC 8032 vector also used above).
const OTHER_KEY: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";

#[test]
fn without_a_paper_master_signing_stays_per_machine_opt_in() {
    let unsigned = publish::unrostered_signature_policy(None).unwrap();
    assert!(!unsigned.required);
    assert_eq!(unsigned.pubkey, None);

    let opted_in = publish::unrostered_signature_policy(Some(OTHER_KEY)).unwrap();
    assert!(opted_in.required);
    assert_eq!(opted_in.pubkey.as_deref(), Some(OTHER_KEY));
}

#[test]
fn appcast_listing_parser_is_lossless_for_relevant_metadata() {
    let rows = r#"{"release_id":55,"tag":"v0.55.0","draft":false,"target_commitish":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","assets":[{"id":1,"name":"aterm-appcast.toml"},{"id":2,"name":"aterm-appcast.toml.sig"}]}
{"release_id":54,"tag":"v0.54.0","draft":false,"target_commitish":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","assets":[{"id":3,"name":"aterm-appcast-v0.54.0.toml"},{"id":4,"name":"aterm-appcast-v0.54.0.toml.sig"}]}
{"release_id":56,"tag":"v0.56.0","draft":true,"target_commitish":"cccccccccccccccccccccccccccccccccccccccc","assets":[{"id":5,"name":"aterm-appcast.toml"}]}
"#;
    let parsed = publish::parse_appcast_asset_listing(rows).unwrap();
    assert_eq!(parsed.len(), 3);
    assert_eq!(
        parsed[0].assets,
        [
            publish::AppcastAsset {
                id: 1,
                name: manifest_out::MANIFEST_ASSET.into(),
            },
            publish::AppcastAsset {
                id: 2,
                name: manifest_out::MANIFEST_SIG_ASSET.into(),
            },
        ]
    );
    assert_eq!(
        parsed[1].assets,
        [
            publish::AppcastAsset {
                id: 3,
                name: "aterm-appcast-v0.54.0.toml".into(),
            },
            publish::AppcastAsset {
                id: 4,
                name: "aterm-appcast-v0.54.0.toml.sig".into(),
            },
        ]
    );
    assert!(parsed[2].draft);
    assert!(publish::parse_appcast_asset_listing("broken row\n").is_err());
}

#[test]
fn appcast_listing_preserves_duplicates_for_fail_closed_replay() {
    let rows = r#"{"release_id":55,"tag":"v0.55.0","draft":false,"target_commitish":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","assets":[{"id":1,"name":"aterm-appcast.toml"},{"id":2,"name":"aterm-appcast.toml"},{"id":3,"name":"aterm-appcast.toml.sig"}]}
"#;
    let parsed = publish::parse_appcast_asset_listing(rows).unwrap();
    assert_eq!(parsed[0].assets.len(), 3);
    let err = publish::verify_channel_head_signature_with(
        &parsed,
        "v0.55.0",
        b"",
        None,
        OTHER_KEY,
        |_, _, _, _| panic!("an ambiguous head is refused before any download"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("duplicate assets"), "{err}");
}

fn yank_published(tag: &str, build: u64, min_build: Option<u64>) -> verify::Published {
    let version = tag.trim_start_matches('v');
    let commit = "a".repeat(40);
    verify::Published {
        release_id: Some(build),
        release: None,
        tag: tag.into(),
        build,
        version: version.into(),
        asset: manifest_out::MANIFEST_ASSET.into(),
        min_build,
        text: format!(
            "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\ncommit = \"{commit}\"\n\
             dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n{}",
            "0".repeat(64),
            min_build.map_or_else(String::new, |floor| format!("min_build = {floor}\n"))
        ),
    }
}

struct HistoricalTagGit {
    output: Mutex<Option<ledger::RunOut>>,
}

impl HistoricalTagGit {
    fn with_stdout(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            output: Mutex::new(Some(ledger::RunOut {
                status: 0,
                stdout: stdout.into(),
                stderr: Vec::new(),
            })),
        }
    }
}

impl ledger::GitRunner for HistoricalTagGit {
    fn git(&self, args: &[&str]) -> ledger::Result<ledger::RunOut> {
        assert_eq!(&args[..3], &["ls-remote", "--tags", "origin"]);
        self.output
            .lock()
            .expect("historical tag output lock")
            .take()
            .ok_or_else(|| ledger::Error::new("unexpected repeated tag query"))
    }
}

#[test]
fn the_head_tag_binding_accepts_the_exact_annotated_ref() {
    let tag_object = "1".repeat(40);
    let commit = "a".repeat(40);
    let output = format!("{tag_object}\trefs/tags/v0.25.0\n{commit}\trefs/tags/v0.25.0^{{}}\n");
    let git = HistoricalTagGit::with_stdout(output.into_bytes());
    publish::assert_remote_annotated_tag_commit(&git, "v0.25.0", &commit).unwrap();
}

#[test]
fn the_head_tag_binding_rejects_missing_lightweight_wrong_and_malformed_refs() {
    let commit = "a".repeat(40);
    let wrong = "b".repeat(40);
    let tag_object = "1".repeat(40);
    let missing = HistoricalTagGit::with_stdout(Vec::new());
    assert!(publish::assert_remote_annotated_tag_commit(&missing, "v0.25.0", &commit).is_err());

    let lightweight =
        HistoricalTagGit::with_stdout(format!("{commit}\trefs/tags/v0.25.0\n").into_bytes());
    assert!(publish::assert_remote_annotated_tag_commit(&lightweight, "v0.25.0", &commit).is_err());

    let wrong_git = HistoricalTagGit::with_stdout(
        format!("{tag_object}\trefs/tags/v0.25.0\n{wrong}\trefs/tags/v0.25.0^{{}}\n").into_bytes(),
    );
    assert!(publish::assert_remote_annotated_tag_commit(&wrong_git, "v0.25.0", &commit).is_err());

    let malformed_git = HistoricalTagGit::with_stdout(
        format!("{commit}\trefs/tags/v0.25.0\n{commit}\trefs/tags/v0.25.0^{{}}\n").into_bytes(),
    );
    assert!(
        publish::assert_remote_annotated_tag_commit(&malformed_git, "v0.25.0", &commit).is_err()
    );
}

#[test]
fn published_snapshot_preserves_symbolic_target_but_claim_capability_does_not() {
    let manifest_commit = "a".repeat(40);
    let historical = publish::ReleaseObjectIdentity {
        id: 349_821_802,
        tag: "v0.25.0".into(),
        draft: false,
        target_commitish: "main".into(),
    };
    publish::validate_release_object_snapshot(Some(&historical), &historical).unwrap();

    assert!(
        publish::validate_release_object_capability(
            Some(&historical),
            historical.id,
            &historical.tag,
            &manifest_commit,
            false,
        )
        .is_err()
    );
    let mut drifted = historical.clone();
    drifted.target_commitish = "Main".into();
    assert!(
        publish::validate_release_object_snapshot(Some(&drifted), &historical).is_err(),
        "symbolic git targets are case-sensitive mutation capabilities"
    );

    let current = publish::ReleaseObjectIdentity {
        id: 55,
        tag: "v0.55.0".into(),
        draft: false,
        target_commitish: manifest_commit.to_ascii_uppercase(),
    };
    publish::validate_release_object_capability(
        Some(&current),
        current.id,
        &current.tag,
        &manifest_commit,
        false,
    )
    .unwrap();
}

#[test]
fn a_channel_release_is_valid_with_a_default_branch_target() {
    let claim = "d".repeat(40);
    let mirrored = publish::ReleaseObjectIdentity {
        id: 360_201_027,
        tag: "v0.7.0".into(),
        draft: false,
        target_commitish: "main".into(),
    };

    publish::validate_release_object_snapshot(Some(&mirrored), &mirrored).unwrap();
    publish::validate_release_object_tag_state(Some(&mirrored), mirrored.id, "v0.7.0", false)
        .unwrap();
    assert!(
        publish::validate_release_object_capability(
            Some(&mirrored),
            mirrored.id,
            &mirrored.tag,
            &claim,
            false,
        )
        .is_err(),
        "a claim SHA can never equal a channel's default branch name"
    );
}

#[test]
fn release_identity_parsers_do_not_lowercase_symbolic_targets() {
    let response =
        br#"{"id":55,"tag_name":"v0.55.0","draft":false,"target_commitish":"Feature/Case"}"#;
    assert_eq!(
        publish::parse_release_object_response(response)
            .unwrap()
            .target_commitish,
        "Feature/Case"
    );
    let rows = "55\tv0.55.0\tfalse\tFeature/Case\n";
    assert_eq!(
        publish::parse_release_object_identity_rows(rows).unwrap()[0].target_commitish,
        "Feature/Case"
    );
}

#[test]
fn yank_successor_first_requires_newer_order_build_and_checked_floor() {
    let bad = yank_published("v0.54.0", 54, None);
    let successor = yank_published("v0.55.0", 55, Some(55));
    assert!(verify::yank_successor_covers(&bad, &successor).unwrap());
    assert!(
        !verify::yank_successor_covers(&bad, &yank_published("v0.55.0", 55, Some(54))).unwrap()
    );
    assert!(
        !verify::yank_successor_covers(&bad, &yank_published("v0.53.0", 56, Some(55))).unwrap()
    );
    assert!(verify::yank_successor_covers(&bad, &yank_published("v0.55.0", 54, Some(55))).is_err());

    let overflow = yank_published("v0.54.0", u64::MAX, None);
    assert!(verify::yank_successor_covers(&overflow, &successor).is_err());
    let mut mismatched = successor.clone();
    mismatched.version = "0.56.0".into();
    assert!(verify::yank_successor_covers(&bad, &mismatched).is_err());

    let retired = yank_published("v0.61", 61, Some(61));
    for (bad_end, successor_end) in [(&retired, &successor), (&bad, &retired)] {
        assert!(
            verify::yank_successor_covers(bad_end, successor_end).is_err(),
            "{} → {}",
            bad_end.tag,
            successor_end.tag
        );
    }
}

fn appcast(version: &str, build: u64) -> Vec<u8> {
    appcast_with_floor(version, build, None)
}

fn appcast_with_floor(version: &str, build: u64, min_build: Option<u64>) -> Vec<u8> {
    let floor = min_build.map_or_else(String::new, |floor| format!("min_build = {floor}\n"));
    format!(
        "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\n\
         dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n{floor}",
        "0".repeat(64),
    )
    .into_bytes()
}

fn release_metadata_row(tag: &str, draft: bool, exact_count: usize) -> String {
    format!("{tag}\t{draft}\t{exact_count}")
}

#[test]
fn client_arbitration_is_permutation_invariant_and_skips_older_503() {
    let rows = [
        release_metadata_row("v0.9.0", false, 1),
        release_metadata_row("v0.10.0", false, 1),
        release_metadata_row("v0.8.0", false, 1),
    ];
    for order in [
        [0usize, 1usize, 2usize],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let listing = order.map(|index| rows[index].as_str()).join("\n");
        let mut fetched = Vec::new();
        let (release_count, scanned) =
            verify::scan_release_page(&listing, true, |_, tag, asset| {
                fetched.push((tag.to_string(), asset.to_string()));
                match tag {
                    "v0.10.0" => Ok(appcast_with_floor("0.10.0", 10, Some(9))),
                    "v0.9.0" | "v0.8.0" => {
                        Err(ledger::Error::new(format!("{tag} appcast: HTTP 503")))
                    }
                    other => panic!("unexpected tag {other}"),
                }
            })
            .expect("canonical maximum must complete without historical fetches");

        assert_eq!(release_count, 3);
        assert_eq!(
            fetched,
            [("v0.10.0".to_string(), manifest_out::MANIFEST_ASSET.into())]
        );
        assert_eq!(scanned.len(), 1);
        assert_eq!((scanned[0].tag.as_str(), scanned[0].build), ("v0.10.0", 10));
        assert_eq!(scanned[0].min_build, Some(9));
    }
}

#[test]
fn client_arbitration_tolerates_real_lower_numeric_legacy_heads() {
    let legacy_tags = [
        "v0.21.2607041853",
        "v0.20.2607041751",
        "v0.19.2607040807",
        "v0.18.2607040011",
        "v0.17.2607032327",
        "v0.15.2607031838",
        "v0.15.2607021856",
        "v0.5.14",
        "v0.5.13",
        "v0.5.12",
        "v0.5.11",
        "v0.5.10",
        "v0.5.9",
    ];
    let mut rows: Vec<String> = legacy_tags
        .iter()
        .rev()
        .map(|tag| release_metadata_row(tag, false, 1))
        .collect();
    rows.insert(6, release_metadata_row("v0.54.0", false, 1));
    let listing = rows.join("\n");
    let mut fetched = Vec::new();
    let (_, scanned) = verify::scan_release_page(&listing, true, |_, tag, asset| {
        fetched.push((tag.to_string(), asset.to_string()));
        match tag {
            "v0.54.0" => Ok(appcast("0.54.0", 540)),
            legacy => panic!("legacy authority was fetched: {legacy}"),
        }
    })
    .expect("lower numeric legacy tags are provably historical");

    assert_eq!(
        fetched,
        [("v0.54.0".to_string(), manifest_out::MANIFEST_ASSET.into())]
    );
    assert_eq!(
        (scanned[0].tag.as_str(), scanned[0].build),
        ("v0.54.0", 540)
    );
}

#[test]
fn client_arbitration_refuses_unorderable_tags() {
    for rejected in [
        "release-old",
        "v0.54.0.1",
        "v01.54.0",
        "v0.54.00",
        "v0.54.",
        "v0",
        "0.54.0",
    ] {
        let listing = [
            release_metadata_row("v0.54.0", false, 1),
            release_metadata_row(rejected, false, 1),
        ]
        .join("\n");
        let mut fetched = false;
        verify::scan_release_page(&listing, true, |_, _, _| {
            fetched = true;
            Ok(appcast("0.54.0", 540))
        })
        .unwrap_err();
        assert!(
            !fetched,
            "{rejected:?}: metadata refusal must precede every download"
        );
    }
}

#[test]
fn client_arbitration_skips_retired_two_component_releases() {
    let listing = [
        release_metadata_row("v0.61", false, 1),
        release_metadata_row("v0.2.0", false, 1),
        release_metadata_row("v0.25", false, 1),
    ]
    .join("\n");
    let mut fetched = Vec::new();
    let (release_count, scanned) = verify::scan_release_page(&listing, true, |_, tag, asset| {
        fetched.push((tag.to_string(), asset.to_string()));
        match tag {
            "v0.2.0" => Ok(appcast("0.2.0", 1_790_000_000)),
            retired => panic!("a retired two-component release was fetched: {retired}"),
        }
    })
    .expect("retired releases are archive history, never an error");

    assert_eq!(
        release_count, 3,
        "every row is still counted for pagination"
    );
    assert_eq!(
        fetched,
        [("v0.2.0".to_string(), manifest_out::MANIFEST_ASSET.into())]
    );
    assert_eq!(
        (scanned[0].tag.as_str(), scanned[0].build),
        ("v0.2.0", 1_790_000_000)
    );

    let retired_only = [
        release_metadata_row("v0.61", false, 1),
        release_metadata_row("v0.25", false, 1),
    ]
    .join("\n");
    let (_, none) = verify::scan_release_page(&retired_only, true, |_, tag, _| {
        panic!("retired-only page must fetch nothing, got {tag}")
    })
    .expect("a retired-only page is empty, not malformed");
    assert!(none.is_empty());
}

#[test]
fn client_arbitration_rejects_malformed_and_duplicate_metadata_before_fetch() {
    let fixtures = [
        release_metadata_row("v0.54.0", false, 2),
        [
            release_metadata_row("v0.54.0", false, 1),
            release_metadata_row("v0.54.0", false, 1),
        ]
        .join("\n"),
        "v0.54.0\tfalse\tnot-a-count".to_string(),
        "v0.54.0\tfalse".to_string(),
    ];
    for listing in fixtures {
        let mut fetched = false;
        verify::scan_release_page(&listing, true, |_, _, _| {
            fetched = true;
            Ok(appcast("0.54.0", 540))
        })
        .unwrap_err();
        assert!(!fetched, "metadata refusal must precede every download");
    }
}

#[test]
fn authoritative_fetch_and_version_mismatch_never_fall_back() {
    let listing = [
        release_metadata_row("v0.9.0", false, 1),
        release_metadata_row("v0.10.0", false, 1),
    ]
    .join("\n");

    let mut fetched = Vec::new();
    let err = verify::scan_release_page(&listing, true, |_, tag, _| {
        fetched.push(tag.to_string());
        match tag {
            "v0.10.0" => Err(ledger::Error::new("authoritative appcast: HTTP 503")),
            older => panic!("fallback fetched after authoritative failure: {older}"),
        }
    })
    .unwrap_err()
    .to_string();
    assert_eq!(fetched, ["v0.10.0"]);
    assert!(err.contains("HTTP 503"), "{err}");

    fetched.clear();
    verify::scan_release_page(&listing, true, |_, tag, _| {
        fetched.push(tag.to_string());
        match tag {
            "v0.10.0" => Ok(appcast("0.9.0", 10)),
            older => panic!("fallback fetched after version rejection: {older}"),
        }
    })
    .unwrap_err();
    assert_eq!(fetched, ["v0.10.0"]);
}

#[test]
fn exhaustive_page_preserves_all_published_appcasts() {
    let listing = [
        release_metadata_row("v0.55.0", true, 1),
        release_metadata_row("v0.54.0", false, 1),
        release_metadata_row("v0.53.0", false, 0),
        release_metadata_row("v0.41.0", false, 1),
    ]
    .join("\n");
    let mut fetched = Vec::new();
    let (page_len, scanned) = verify::scan_release_page(&listing, false, |_, tag, asset| {
        fetched.push((tag.to_string(), asset.to_string()));
        match tag {
            "v0.54.0" => Ok(appcast("0.54.0", 540)),
            "v0.41.0" => Ok(appcast("0.41.0", 410)),
            other => panic!("draft/appcast-less tag was fetched: {other}"),
        }
    })
    .expect("exhaustive status scan");

    assert_eq!(page_len, 4);
    assert_eq!(
        fetched,
        [
            ("v0.54.0".to_string(), manifest_out::MANIFEST_ASSET.into()),
            ("v0.41.0".to_string(), manifest_out::MANIFEST_ASSET.into()),
        ]
    );
    assert_eq!(
        scanned
            .iter()
            .map(|published| (published.tag.as_str(), published.build))
            .collect::<Vec<_>>(),
        [("v0.54.0", 540), ("v0.41.0", 410)]
    );
    assert_eq!(
        verify::select_newest(&scanned).map(|published| published.tag.as_str()),
        Some("v0.54.0")
    );
}

#[test]
fn exhaustive_history_rejects_duplicate_exact_names() {
    let listing = release_metadata_row("v0.54.0", false, 2);
    let mut fetched = false;
    let err = verify::scan_release_page(&listing, false, |_, _, _| {
        fetched = true;
        Ok(appcast("0.54.0", 540))
    })
    .unwrap_err()
    .to_string();
    assert!(!fetched);
    assert!(err.contains("duplicate assets"), "{err}");
}

fn parse(args: &[&str]) -> Result<cli::Cmd, String> {
    cli::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

#[test]
fn cli_refuses_invocations_that_would_drop_a_safety_answer() {
    let sha = "a".repeat(40);
    for bad in [
        &["recover", "v0.55.0", sha.as_str()][..],
        &["yank", "1783918101"],
        &["yank", "1", "--mac-only", "--linux-artifacts", "/handoff"],
        &[
            "yank",
            "1",
            "--release-credentials",
            "/keys/m3.toml",
            "--release-credentials",
            "/keys/m4.toml",
            "--mac-only",
        ],
        &["cut", "--resume", "--dry-run"],
        &["release", "--bump", "--commit", "a"],
        &["release", "--commit", "a", "--commit", "b"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn recovery_requires_the_external_stop_precondition() {
    let owner = "a".repeat(40);
    let error = publish::run_recover_lost(
        &PathBuf::from("/definitely/not/an/aterm/repository"),
        "0.55.0",
        &owner,
        false,
        false,
        None,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, publish::RECOVERY_STOPPED_PROCESS_REFUSAL);
}
