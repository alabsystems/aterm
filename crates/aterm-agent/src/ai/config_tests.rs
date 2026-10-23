// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_absent_table_is_every_default_and_every_cli_reviews() {
    let (local, notes) = read("[harness]\napprove = \"safe\"\n");
    assert_eq!(local, Local::default());
    assert!(notes.is_empty(), "{notes:?}");
    let s = resolve(&local, None, None, notes);
    assert!(s.enabled);
    assert_eq!(s.preferred.0.id, default_preferred().id);
    assert_eq!(s.preferred.1, From::Default);
    assert_eq!(s.reviews, (true, From::Default), "batteries included");
    assert_eq!(
        s.reviewers.0.iter().map(|c| c.id).collect::<Vec<_>>(),
        connector::ids(),
        "every CLI reviews"
    );
    assert_eq!(s.review_permissions, (Choice::Inherit, From::Default));
    assert_eq!(s.review_count, (REVIEW_COUNT_DEFAULT, From::Default));
    assert_eq!(s.review_timeout_s, (TIMEOUT_DEFAULT_S, From::Default));
    assert!(
        s.uses
            .iter()
            .all(|(_, on, from)| *on && *from == From::Default)
    );
    assert_eq!(s.fleet, FleetState::None);
}

#[test]
fn every_key_reads_and_a_bad_value_is_a_note_and_its_default() {
    let text = "[ai]\nenabled = false\nreviews = false\npreferred = \"codex\"\nreviewers = \
                [\"claude\", \"agy\", \"claude\"]\nreview_count = 2\nreview_timeout_s = 900\n\
                review_permissions = \"plan\"\nmachine = \"m7\"\nfleet = \
                \"https://example.com/ai-fleet.toml\"\nfleet_key = \
                \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n[ai.muse]\nuse = false\n";
    let (local, notes) = read(text);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(local.enabled, Some(false));
    assert_eq!(local.machine.as_deref(), Some("m7"));
    assert_eq!(local.layer.preferred, Some("codex"));
    assert_eq!(local.layer.reviewers, Some(vec!["claude", "agy"]));
    assert_eq!(local.layer.review_count, Some(2));
    assert_eq!(local.layer.review_timeout_s, Some(900));
    assert_eq!(local.layer.reviews, Some(false));
    assert_eq!(
        local.layer.review_permissions,
        Some(Choice::Fixed(crate::ai::permission::Level::Plan))
    );
    assert_eq!(
        local.fleet,
        Some(FleetSource::Url("https://example.com/ai-fleet.toml".into()))
    );
    assert!(local.fleet_key.is_some());
    assert_eq!(local.layer.uses.get("muse"), Some(&false));
    let s = resolve(&local, None, None, Vec::new());
    assert!(!s.wants("muse"));
    assert_eq!(
        s.reviewers.0.iter().map(|c| c.id).collect::<Vec<_>>(),
        ["claude", "agy"]
    );

    let (bad, notes) = read(
        "[ai]\npreferred = \"gpt\"\nreview_count = 9\nfleet = \"relative.toml\"\nmachine = \
         \"Work Mac\"\nwhatever = 1\nreview_permissions = \"root\"\nfleet_key = \"abc\"\n\
         reviews = \"yes\"\n[ai.codex]\nlogin = \"x\"\n",
    );
    assert_eq!(bad, Local::default());
    assert_eq!(notes.len(), 9, "{notes:?}");
    assert!(
        notes.iter().any(|n| n.contains("names no AI CLI")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("only `use` is per CLI")),
        "{notes:?}"
    );
    let (bad, notes) = read("[ai]\nfleet = \"http://example.com/f.toml\"\n");
    assert_eq!(
        (bad.fleet, notes.len()),
        (None, 1),
        "plain http is no source"
    );
}

#[test]
fn a_file_that_is_not_toml_reads_as_an_empty_table_with_a_note() {
    let (local, notes) = read("[ai\npreferred = ");
    assert_eq!(local, Local::default());
    assert_eq!(notes.len(), 1);
}

const FLEET: &str = "schema = 1\npreferred = \"codex\"\nreview_count = 3\nreviewers = \
                     [\"codex\", \"claude\"]\n[agy]\nuse = false\n[machine.m7]\npreferred = \
                     \"muse\"\n[machine.m7.agy]\nuse = true\n";

#[test]
fn a_fleet_document_layers_under_the_local_table() {
    let (fleet, notes) = read_fleet(FLEET).expect("admitted");
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(fleet.root.preferred, Some("codex"));
    assert_eq!(fleet.machines["m7"].preferred, Some("muse"));
    let path = &FleetSource::Path(PathBuf::from("/tmp/fleet.toml"));

    let (local, notes) = read("[ai]\nmachine = \"m7\"\nreview_count = 1\n");
    let s = resolve(
        &local,
        Some(path),
        Some(Ok((fleet.clone(), Vec::new()))),
        notes,
    );
    assert_eq!(
        (s.preferred.0.id, s.preferred.1),
        ("muse", From::FleetMachine)
    );
    assert_eq!(s.review_count, (1, From::Local));
    let agy = s.uses.iter().find(|(c, _, _)| c.id == "agy").expect("agy");
    assert_eq!((agy.1, agy.2), (true, From::FleetMachine));
    assert!(matches!(s.fleet, FleetState::Read { row: true, .. }));

    let (other, notes) = read("[ai]\nmachine = \"m11\"\n");
    let s = resolve(&other, Some(path), Some(Ok((fleet, Vec::new()))), notes);
    assert_eq!((s.preferred.0.id, s.preferred.1), ("codex", From::Fleet));
    assert!(!s.wants("agy"));
    assert!(
        s.notes.iter().any(|n| n.contains("no [machine.m11] row")),
        "{:?}",
        s.notes
    );
}

#[test]
fn a_fleet_document_names_the_reviewers_under_the_local_table() {
    let (fleet, _) = read_fleet(FLEET).expect("admitted");
    let path = &FleetSource::Path(PathBuf::from("/tmp/fleet.toml"));
    let (local, notes) = read("[ai]\n");
    let s = resolve(
        &local,
        Some(path),
        Some(Ok((fleet.clone(), Vec::new()))),
        notes,
    );
    assert_eq!(
        s.reviewers.0.iter().map(|c| c.id).collect::<Vec<_>>(),
        ["codex", "claude"]
    );
    assert_eq!(s.reviewers.1, From::Fleet);
    let (local, notes) = read("[ai]\nreviewers = [\"agy\"]\n");
    let s = resolve(&local, Some(path), Some(Ok((fleet, Vec::new()))), notes);
    assert_eq!(
        (
            s.reviewers.0.iter().map(|c| c.id).collect::<Vec<_>>(),
            s.reviewers.1
        ),
        (vec!["agy"], From::Local)
    );
}

#[test]
fn reviews_and_their_permissions_come_from_the_table_or_the_document() {
    let (local, notes) = read("[ai]\nreviews = false\n");
    assert!(notes.is_empty(), "{notes:?}");
    let s = resolve(&local, None, None, notes);
    assert_eq!(s.reviews, (false, From::Local));
    let (fleet, notes) =
        read_fleet("schema = 1\nreviews = false\nreview_permissions = \"edits\"\n").expect("read");
    assert!(notes.is_empty(), "{notes:?}");
    let path = &FleetSource::Path(PathBuf::from("/tmp/fleet.toml"));
    let s = resolve(
        &Local::default(),
        Some(path),
        Some(Ok((fleet, Vec::new()))),
        Vec::new(),
    );
    assert_eq!(s.reviews, (false, From::Fleet));
    assert_eq!(
        s.review_permissions,
        (
            Choice::Fixed(crate::ai::permission::Level::Edits),
            From::Fleet
        )
    );
}

#[cfg(unix)]
#[test]
fn a_fleet_document_others_may_write_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("aterm.toml");
    std::fs::write(&config, "[ai]\n").expect("config");
    let fleet = dir.path().join(FLEET_FILE);
    std::fs::write(&fleet, "schema = 1\npreferred = \"codex\"\n").expect("fleet");
    std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(
        load(Some(&config)).preferred.0.id,
        "codex",
        "the control: a private file is read"
    );
    std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let s = load(Some(&config));
    assert!(
        matches!(s.fleet, FleetState::Refused { ref why, .. } if why == "writable by others"),
        "{:?}",
        s.fleet
    );
    assert_eq!(s.preferred.1, From::Default);
}

/// Ubuntu's umask `002` writes every document `0664` on its user's private group, whose
/// write bit is no other account's: on each group this process holds, a group-writable
/// document is read when the group is its user's private one and refused on any other, and
/// a world-writable one is refused on every group.
#[cfg(unix)]
#[test]
fn a_group_writable_fleet_document_is_read_only_on_its_owners_private_group() {
    use aterm_uds::shared_write::is_user_private_group;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("aterm.toml");
    std::fs::write(&config, "[ai]\n").expect("config");
    let fleet = dir.path().join(FLEET_FILE);
    std::fs::write(&fleet, "schema = 1\npreferred = \"codex\"\n").expect("fleet");
    // SAFETY: getgid has no preconditions and cannot fail.
    let mut gids = vec![unsafe { libc::getgid() }];
    // SAFETY: a zero count asks only for the number of supplementary groups.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut more = vec![0; usize::try_from(count).expect("a group count")];
    // SAFETY: `more` has room for exactly `count` gids.
    let got = unsafe { libc::getgroups(count, more.as_mut_ptr()) };
    more.truncate(usize::try_from(got).expect("the groups"));
    for gid in more {
        if !gids.contains(&gid) {
            gids.push(gid);
        }
    }
    for gid in gids {
        std::os::unix::fs::chown(&fleet, None, Some(gid)).expect("a group of ours");
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o664)).expect("chmod");
        let s = load(Some(&config));
        if is_user_private_group(gid) {
            assert_eq!(
                (s.preferred.0.id, s.preferred.1),
                ("codex", From::Fleet),
                "gid {gid} is its owner's private group: {:?}",
                s.fleet
            );
        } else {
            assert!(
                matches!(s.fleet, FleetState::Refused { ref why, .. } if why == "writable by others"),
                "gid {gid} is no private group: {:?}",
                s.fleet
            );
        }
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o666)).expect("chmod");
        let s = load(Some(&config));
        assert!(
            matches!(s.fleet, FleetState::Refused { ref why, .. } if why == "writable by others"),
            "gid {gid}, world-writable: {:?}",
            s.fleet
        );
    }
}

/// A POSIX access ACL makes a file's group bits its mask: a `0664` document on its owner's
/// private group that also lets another account write it (`setfacl -m u:nobody:rw`, the mode
/// unchanged) is refused, where without the ACL it is read.
#[cfg(target_os = "linux")]
#[test]
fn a_fleet_document_an_access_acl_shares_is_refused_on_the_private_group_too() {
    use aterm_uds::shared_write::{grant_group_class_by_acl, is_user_private_group};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("aterm.toml");
    std::fs::write(&config, "[ai]\n").expect("config");
    let fleet = dir.path().join(FLEET_FILE);
    std::fs::write(&fleet, "schema = 1\npreferred = \"codex\"\n").expect("fleet");
    std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    let gid = std::fs::metadata(&fleet).expect("stat").gid();
    if is_user_private_group(gid) {
        assert_eq!(
            load(Some(&config)).preferred.1,
            From::Fleet,
            "the control: without the ACL, gid {gid} (private) is read"
        );
    }
    grant_group_class_by_acl(&fleet, 65534).expect("an access ACL");
    assert_eq!(
        std::fs::metadata(&fleet).expect("stat").mode() & 0o777,
        0o664,
        "the ACL leaves the mode bits as they were"
    );
    let s = load(Some(&config));
    assert!(
        matches!(s.fleet, FleetState::Refused { ref why, .. } if why == "writable by others"),
        "gid {gid}, an access ACL: {:?}",
        s.fleet
    );
    assert_eq!(s.preferred.1, From::Default);
}

#[test]
fn the_window_switch_is_never_read_from_a_fleet_document() {
    let (_, notes) = read_fleet("schema = 1\nreviews = true\nenabled = false\n").expect("read");
    assert_eq!(notes.len(), 1, "{notes:?}");
    let (_, notes) = read_fleet("schema = 1\nseq = 4\nvalid_until = 2000000000\n").expect("read");
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn a_fleet_document_written_for_a_newer_build_still_applies() {
    let (fleet, notes) = read_fleet(
        "schema = 1\npreferred = \"codex\"\nnew_key = 3\nreviewers = [\"codex\", \"gemini\"]\n\
         [gemini]\nuse = true\n[machine.m7]\nfuture = \"x\"\n[machine.M7]\npreferred = \"codex\"\n",
    )
    .expect("read");
    assert_eq!(fleet.root.preferred, Some("codex"));
    assert_eq!(fleet.root.reviewers, Some(vec!["codex"]));
    assert!(fleet.machines.contains_key("m7"));
    assert!(!fleet.machines.contains_key("M7"));
    assert_eq!(notes.len(), 5, "{notes:?}");
}

#[test]
fn only_text_that_is_not_toml_or_another_schema_refuses_a_document() {
    for (text, why) in [
        ("preferred = \"codex\"\n", "no `schema = 1`"),
        ("schema = 2\n", "schema 2"),
        ("schema = [\n", "not TOML"),
    ] {
        let got = read_fleet(text).expect_err(text);
        assert!(got.contains(why), "{text:?}: {got}");
    }
    let (local, notes) = read("[ai]\n");
    let s = resolve(
        &local,
        Some(&FleetSource::Path(PathBuf::from("/tmp/f.toml"))),
        Some(Err("schema 2".to_string())),
        notes,
    );
    assert_eq!(s.preferred.1, From::Default);
    assert!(matches!(s.fleet, FleetState::Refused { ref why, .. } if why == "schema 2"));
}

#[test]
fn load_reads_the_named_document_or_the_one_beside_aterm_toml() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("aterm.toml");
    std::fs::write(dir.path().join(FLEET_FILE), FLEET).expect("fleet");
    std::fs::write(&config, "[ai]\nmachine = \"m7\"\n").expect("config");
    let s = load(Some(&config));
    assert_eq!(s.preferred.0.id, "muse");
    let other = dir.path().join("elsewhere.toml");
    std::fs::write(&other, "schema = 1\npreferred = \"codex\"\n").expect("other");
    std::fs::write(
        &config,
        format!("[ai]\nfleet = {:?}\n", other.display().to_string()),
    )
    .expect("config");
    let s = load(Some(&config));
    assert_eq!(s.preferred.0.id, "codex");
    std::fs::remove_file(&other).expect("rm");
    let s = load(Some(&config));
    assert!(
        matches!(s.fleet, FleetState::Refused { .. }),
        "{:?}",
        s.fleet
    );
    assert_eq!(s.preferred.1, From::Default);
    let s = load(Some(&dir.path().join("sub").join("absent.toml")));
    assert_eq!((s.preferred.1, s.fleet), (From::Default, FleetState::None));
}

#[test]
fn a_refused_fleet_document_applies_nothing() {
    let (local, _) = read("[ai]\nreviewers = [\"claude\", \"codex\"]\n");
    let path = &FleetSource::Path(PathBuf::from("/private-fixture/ai-fleet.toml"));
    for why in ["permission denied", "no such file", "not UTF-8", "schema 2"] {
        let s = resolve(&local, Some(path), Some(Err(why.into())), Vec::new());
        assert!(matches!(s.fleet, FleetState::Refused { .. }));
        assert_eq!(
            (s.reviewers.0.len(), s.reviewers.1),
            (2, From::Local),
            "{why}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_dangling_default_fleet_entry_is_refused_not_absent() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let config = dir.path().join("aterm.toml");
    std::fs::write(&config, "[ai]\n").expect("config");
    assert_eq!(
        load(Some(&config)).fleet,
        FleetState::None,
        "genuine absence"
    );
    let fleet = dir.path().join(FLEET_FILE);
    std::os::unix::fs::symlink(dir.path().join("missing-fleet"), &fleet).expect("dangling link");
    assert!(matches!(
        load(Some(&config)).fleet,
        FleetState::Refused { .. }
    ));
}

/// A signed fleet document under a new key: (public key, secret key).
fn signer() -> (String, Vec<u8>) {
    let (secret, public) = crate::ai::fleet::keygen().expect("keygen");
    (public, secret)
}

#[cfg(unix)]
#[test]
fn a_file_under_a_pinned_key_must_carry_its_signature() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let doc = dir.path().join("fleet.toml");
    let text = "schema = 1\npreferred = \"codex\"\n";
    std::fs::write(&doc, text).expect("doc");
    std::fs::set_permissions(&doc, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let (public, secret) = signer();
    let sig = dir.path().join("fleet.toml.sig");
    let got = load_fleet(&doc, Some(&public)).expect_err("unsigned");
    assert!(got.contains("signature"), "{got}");
    let signed = crate::ai::fleet::sign(&secret, text.as_bytes()).expect("sign");
    std::fs::write(&sig, &signed).expect("sig");
    std::fs::set_permissions(&sig, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let (fleet, _) = load_fleet(&doc, Some(&public)).expect("verified");
    assert_eq!(fleet.root.preferred, Some("codex"));
    std::fs::write(&doc, "schema = 1\npreferred = \"muse\"\n").expect("doc");
    let got = load_fleet(&doc, Some(&public)).expect_err("tampered");
    assert!(got.contains("does not verify"), "{got}");
    assert!(load_fleet(&doc, None).is_ok());
}

#[test]
fn a_url_needs_a_pinned_key_and_reads_only_what_is_held() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let url = FleetSource::Url("https://example.com/ai-fleet.toml".into());
    let got = load_source(&url, None, Some(dir.path()), 0).expect_err("no key");
    assert!(got.contains("fleet_key"), "{got}");
    let (public, _) = signer();
    let got = load_source(&url, Some(&public), Some(dir.path()), 0).expect_err("nothing held");
    assert_eq!(got, "not fetched yet");
    let (local, notes) = read(&format!(
        "[ai]\nfleet = \"https://example.com/ai-fleet.toml\"\nfleet_key = {public:?}\n"
    ));
    let source = fleet_source(&local, None).expect("named");
    let fleet = load_source(&source, local.fleet_key.as_deref(), Some(dir.path()), 0);
    let s = resolve(&local, Some(&source), Some(fleet), notes);
    assert!(
        matches!(&s.fleet, FleetState::Refused { source, why } if source.starts_with("https://") && why == "not fetched yet"),
        "{:?}",
        s.fleet
    );
    assert_eq!(
        s.fleet_source.as_deref(),
        Some("https://example.com/ai-fleet.toml")
    );
}
