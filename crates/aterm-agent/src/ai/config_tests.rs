// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_absent_table_is_every_default_and_reviews_are_off() {
    let (local, notes) = read("[harness]\napprove = \"safe\"\n");
    assert_eq!(local, Local::default());
    assert!(notes.is_empty(), "{notes:?}");
    let s = resolve(&local, None, None, notes);
    assert!(s.enabled);
    assert_eq!(s.preferred.0.id, default_preferred().id);
    assert_eq!(s.preferred.1, From::Default);
    assert!(
        s.reviewers.0.is_empty(),
        "no CLI receives code until the owner lists it"
    );
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
    let text = "[ai]\nenabled = false\npreferred = \"codex\"\nreviewers = [\"claude\", \"agy\", \
                \"claude\"]\nreview_count = 2\nreview_timeout_s = 900\nmachine = \"m7\"\n\
                [ai.muse]\nuse = false\n";
    let (local, notes) = read(text);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(local.enabled, Some(false));
    assert_eq!(local.machine.as_deref(), Some("m7"));
    assert_eq!(local.layer.preferred, Some("codex"));
    assert_eq!(local.layer.reviewers, Some(vec!["claude", "agy"]));
    assert_eq!(local.layer.review_count, Some(2));
    assert_eq!(local.layer.review_timeout_s, Some(900));
    assert_eq!(local.layer.uses.get("muse"), Some(&false));
    let s = resolve(&local, None, None, Vec::new());
    assert!(!s.wants("muse"));
    assert_eq!(
        s.reviewers.0.iter().map(|c| c.id).collect::<Vec<_>>(),
        ["claude", "agy"]
    );

    let (bad, notes) = read(
        "[ai]\npreferred = \"gpt\"\nreview_count = 9\nfleet = \"relative.toml\"\nmachine = \
         \"Work Mac\"\nwhatever = 1\n[ai.codex]\nlogin = \"x\"\n",
    );
    assert_eq!(bad, Local::default());
    assert_eq!(notes.len(), 6, "{notes:?}");
    assert!(
        notes.iter().any(|n| n.contains("names no AI CLI")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("only `use` is per CLI")),
        "{notes:?}"
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
    let path = Path::new("/tmp/fleet.toml");

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

    // Another machine reads the top level, and is told it has no row.
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
fn a_fleet_document_can_only_shorten_the_reviewers() {
    let (fleet, _) = read_fleet(FLEET).expect("admitted");
    let path = Path::new("/tmp/fleet.toml");
    // No local list: the default (empty) leaves nothing for the document to keep.
    let (local, notes) = read("[ai]\n");
    let s = resolve(
        &local,
        Some(path),
        Some(Ok((fleet.clone(), Vec::new()))),
        notes,
    );
    assert!(
        s.reviewers.0.is_empty(),
        "a fleet document never adds a reviewer"
    );
    // A local list with claude only: the document's order, less what the list leaves out.
    let (local, notes) = read("[ai]\nreviewers = [\"claude\"]\n");
    let s = resolve(&local, Some(path), Some(Ok((fleet, Vec::new()))), notes);
    assert_eq!(
        s.reviewers.0.iter().map(|c| c.id).collect::<Vec<_>>(),
        ["claude"]
    );
    assert_eq!(s.reviewers.1, From::Fleet);
    // The control: the document drops a CLI the local list holds (no machine sends code to
    // a vendor the fleet leaves out).
    let (local, notes) = read("[ai]\nreviewers = [\"codex\", \"claude\"]\n");
    let (narrow, _) = read_fleet("schema = 1\nreviewers = [\"claude\"]\n").expect("read");
    let s = resolve(&local, Some(path), Some(Ok((narrow, Vec::new()))), notes);
    assert_eq!(
        s.reviewers.0.iter().map(|c| c.id).collect::<Vec<_>>(),
        ["claude"]
    );
    // With no document the local list stands.
    let (local, notes) = read("[ai]\nreviewers = [\"codex\", \"claude\"]\n");
    let s = resolve(&local, None, None, notes);
    assert_eq!((s.reviewers.0.len(), s.reviewers.1), (2, From::Local));
}

#[test]
fn a_reviews_switch_is_not_a_key() {
    let (local, notes) = read("[ai]\nreviews = false\n");
    assert_eq!(local, Local::default());
    assert_eq!(notes.len(), 1, "{notes:?}");
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

#[test]
fn reviews_is_never_read_from_a_fleet_document() {
    let (_, notes) = read_fleet("schema = 1\nreviews = true\nenabled = false\n").expect("read");
    assert_eq!(notes.len(), 2, "{notes:?}");
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
        Some(Path::new("/tmp/f.toml")),
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
    // Beside aterm.toml, unnamed.
    std::fs::write(dir.path().join(FLEET_FILE), FLEET).expect("fleet");
    std::fs::write(&config, "[ai]\nmachine = \"m7\"\n").expect("config");
    let s = load(Some(&config));
    assert_eq!(s.preferred.0.id, "muse");
    // Named elsewhere.
    let other = dir.path().join("elsewhere.toml");
    std::fs::write(&other, "schema = 1\npreferred = \"codex\"\n").expect("other");
    std::fs::write(
        &config,
        format!("[ai]\nfleet = {:?}\n", other.display().to_string()),
    )
    .expect("config");
    let s = load(Some(&config));
    assert_eq!(s.preferred.0.id, "codex");
    // A named document that is gone is refused, and the defaults stand.
    std::fs::remove_file(&other).expect("rm");
    let s = load(Some(&config));
    assert!(
        matches!(s.fleet, FleetState::Refused { .. }),
        "{:?}",
        s.fleet
    );
    assert_eq!(s.preferred.1, From::Default);
    // No file at all: every default, and no document.
    let s = load(Some(&dir.path().join("sub").join("absent.toml")));
    assert_eq!((s.preferred.1, s.fleet), (From::Default, FleetState::None));
}

#[test]
fn an_unread_fleet_cannot_restore_reviewers_it_may_have_removed() {
    let (local, _) = read("[ai]\nreviewers = [\"claude\", \"codex\"]\n");
    let path = Path::new("/private-fixture/ai-fleet.toml");
    let (fleet, _) = read_fleet("schema = 1\nreviewers = [\"claude\"]\n").expect("fleet");
    let narrowed = resolve(
        &local,
        Some(path),
        Some(Ok((fleet, Vec::new()))),
        Vec::new(),
    );
    assert_eq!(
        narrowed
            .reviewers
            .0
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        ["claude"],
        "the admitted fleet narrows the local egress grant"
    );
    let standalone = resolve(&local, None, None, Vec::new());
    assert_eq!(
        standalone.reviewers.0.len(),
        2,
        "no fleet leaves the local grant"
    );
    for why in ["permission denied", "no such file", "not UTF-8", "schema 2"] {
        let unknown = resolve(&local, Some(path), Some(Err(why.into())), Vec::new());
        assert!(matches!(unknown.fleet, FleetState::Refused { .. }));
        assert!(
            unknown.reviewers.0.is_empty(),
            "an unread narrowing policy cannot restore the local grant: {why}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_dangling_default_fleet_entry_is_unknown_policy_not_no_policy() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let config = dir.path().join("aterm.toml");
    std::fs::write(&config, "[ai]\nreviewers = [\"codex\"]\n").expect("config");
    assert_eq!(load(Some(&config)).reviewers.0.len(), 1, "genuine absence");
    let fleet = dir.path().join(FLEET_FILE);
    std::fs::write(&fleet, "schema = 1\nreviewers = []\n").expect("fleet");
    assert!(
        load(Some(&config)).reviewers.0.is_empty(),
        "admitted empty filter"
    );
    std::fs::remove_file(&fleet).expect("remove own fixture");
    std::os::unix::fs::symlink(dir.path().join("missing-fleet"), &fleet).expect("dangling link");
    let unknown = load(Some(&config));
    assert!(matches!(unknown.fleet, FleetState::Refused { .. }));
    assert!(
        unknown.reviewers.0.is_empty(),
        "the unknown filter still applies"
    );
}
