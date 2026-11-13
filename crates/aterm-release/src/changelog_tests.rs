// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
// Extraction preserves immutable notes; a new cut must satisfy the shared structure.
fn repeated_subsections_preserve_extraction_but_refuse_shipping() {
    let doc = "## [Unreleased]\n\n### Fixed\n\n- first fix\n\n### Security\n\n- a change\n\n### Fixed\n\n- another fix\n\n## [0.1.0] - date\n\n### Fixed\n\n- older fix\n";
    assert!(gate_unreleased(doc).is_err());
    let rolled = roll(doc, "0.2.0", "date").unwrap();
    let expected = doc
        .split_once("## [Unreleased]")
        .unwrap()
        .1
        .split_once("## [0.1.0]")
        .unwrap()
        .0
        .trim();
    assert_eq!(rolled_body(&rolled, "0.2.0").unwrap(), expected);
    let coalesced = doc.replace("### Fixed\n\n- another fix\n\n", "").replacen(
        "- first fix",
        "- first fix\n\n- another fix",
        1,
    );
    assert!(gate_unreleased(&coalesced).is_ok());
    let comment = doc.replacen(
        "### Fixed\n\n- another fix",
        "<!-- ### Fixed -->\n\n- another fix",
        1,
    );
    assert!(gate_unreleased(&comment).is_ok());
    let recut = doc.replacen("## [Unreleased]", "## [0.2.0]", 1);
    assert!(gate_section(&recut, "0.2.0").is_err());
}

#[test]
fn harness_window_provenance_is_not_inferred_from_copied_sections() {
    let doc = "## [Unreleased]\n\n### Changed\n\n- a new fix\n\n- **Harness versions since v0.193.0: `alab claude harness 0.31`.**\n\n## [0.194.0] - date\n\n- `alab claude harness 0.31` records the upgrade changes.\n\n## [0.193.0] - date\n\n- older change\n";
    gate_unreleased(doc).unwrap();
}

#[test]
fn malformed_harness_announcements_still_refuse_without_source_objects() {
    let doc = "## [Unreleased]\n\n- a new fix\n\n- **Harness versions since v0.193.0: `alab claude harness 0.31`.**\n\n## [0.194.0] - date\n\n- `alab claude harness 0.31` records the upgrade changes.\n";
    let dated = doc.replacen("since v0.193.0", "since v0.194.0", 1);
    gate_unreleased(&dated).unwrap();
    assert!(gate_unreleased(&dated.replacen("since v0.194.0:", "since vgarbled:", 1)).is_err());
    let fresh = dated.replacen("harness 0.31", "harness 0.32", 1);
    assert!(gate_unreleased(&fresh).is_ok());
    assert!(gate_unreleased(&fresh.replace("harness 0.32", "harness\n  0.32")).is_ok());
    let recut = dated.replacen("## [Unreleased]", "## [0.195.0]", 1);
    assert!(gate_section(&recut, "0.195.0").is_ok());
}

/// Mutable older sections cannot establish whether an entry shipped. The
/// Git-bound gate judges that against the release's own immutable object.
#[test]
fn section_copies_do_not_claim_shipment_without_release_objects() {
    let doc = "## [Unreleased]\n\n- **Fix.** Same  words.\n  Continued here.\n\n## [0.2.0] - date\n\n- newer unrelated fix\n\n## [0.1.0] - date\n\n* **Fix.** Same words. Continued here.\n";
    assert!(gate_unreleased(doc).is_ok());
    assert!(roll(doc, "0.3.0", "date").is_ok());
    assert!(release_changelog(doc, "0.3.0", "date").is_ok());
    assert!(roll_shipped(doc, doc, "0.3.0", "date").is_ok());
    let recut = doc.replace("## [Unreleased]", "## [0.3.0] - date");
    assert!(gate_section(&recut, "0.3.0").is_ok());
    assert!(release_changelog(&recut, "0.3.0", "date").is_ok());

    // Same bold lead, new description: a legitimate follow-up survives.
    let followup = doc.replacen("Same  words.", "A further fix.", 1);
    assert!(gate_unreleased(&followup).is_ok());
    assert!(roll(&followup, "0.3.0", "date").is_ok());
    // Removing the older entry also changes the verdict.
    assert!(
        gate_unreleased(&doc.replace(
            "* **Fix.** Same words. Continued here.",
            "- distinct older entry"
        ))
        .is_ok()
    );
}

#[test]
fn harness_entries_keep_their_own_rule_and_recuts_ignore_newer_notes() {
    let harness = format!("{HARNESS_ENTRY_HEAD}0.2.0: `alab claude harness 0.32`.**");
    let old_harness = format!("{HARNESS_ENTRY_HEAD}0.1.0: `alab claude harness 0.31`.**");
    let doc = format!(
        "## [Unreleased]\n\n- fresh note\n\n{harness}\n\n## [0.2.0] - date\n\n- old note\n\n{old_harness}\n\n## [0.1.0] - date\n\n- oldest note\n"
    );
    assert!(gate_unreleased(&doc).is_ok());
    let duplicate = doc.replace("fresh note", "old note");
    assert!(gate_unreleased(&duplicate).is_ok());
    assert!(gate_section(&duplicate, "0.2.0").is_ok());
}

/// TODAY IN LOS ANGELES IS WHAT `date` SAYS under `TZ=America/Los_Angeles` (the reader
/// [`today_la`] replaced, as its oracle; read between two of ours, so a midnight between
/// them is no failure). NEGATIVE CONTROL: the zone's offset moves the day, so a midnight
/// UTC is still the day before there.
#[test]
fn today_in_los_angeles_is_what_date_says() {
    assert_eq!(la_date(1_792_627_200, -7 * 3600), "2026-10-21");
    assert_eq!(la_date(1_792_627_200, 0), "2026-10-22");
    let before = today_la().expect("the zone file");
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("date"))
        .env("TZ", "America/Los_Angeles")
        .arg("+%Y-%m-%d")
        .output()
        .expect("run date");
    let after = today_la().expect("the zone file");
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        said == before || said == after,
        "{said} vs {before}..{after}"
    );
}

const SOURCE: &str = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- **One** — shipped.\n  A continuation line.\n\n- **Two** — shipped.\n\n## [0.91.0] - 2026-09-22\n\n- older.\n";

/// Main that has not moved: the roll is exactly [`roll`], byte for byte.
#[test]
fn roll_shipped_of_an_unmoved_main_is_the_plain_roll() {
    assert_eq!(
        roll_shipped(SOURCE, SOURCE, "0.92.0", "2026-09-23").unwrap(),
        roll(SOURCE, "0.92.0", "2026-09-23").unwrap()
    );
}

/// Main moved after the publish: what the published commit carried ships, what was
/// added after stays unreleased, and an edited entry's new text stays while its old
/// text ships.
#[test]
fn roll_shipped_keeps_what_peers_added_after_the_publish_unreleased() {
    let main = SOURCE
        .replace(
            "### Fixed\n\n- **One**",
            "### Added\n\n- **New** — a peer's.\n\n### Fixed\n\n- **Peer fix** — after the publish.\n\n- **One**",
        )
        .replace("- **Two** — shipped.", "- **Two** — shipped, then reworded.");
    let rolled = roll_shipped(&main, SOURCE, "0.92.0", "2026-09-23").unwrap();
    assert_eq!(
        rolled,
        "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- **New** — a peer's.\n\n\
         ### Fixed\n\n- **Peer fix** — after the publish.\n\n\
         - **Two** — shipped, then reworded.\n\n\
         ## [0.92.0] - 2026-09-23\n\n### Fixed\n\n- **One** — shipped.\n  A continuation line.\n\n\
         - **Two** — shipped.\n\n## [0.91.0] - 2026-09-22\n\n- older.\n"
    );
    let plain = roll(&main, "0.92.0", "2026-09-23").unwrap();
    assert!(
        rolled_body(&plain, "0.92.0").unwrap().contains("a peer's"),
        "{plain}"
    );
    assert!(
        !rolled_body(&rolled, "0.92.0").unwrap().contains("a peer's"),
        "{rolled}"
    );
}

#[test]
fn claim_changelogs_takes_an_existing_section_as_it_stands() {
    let rolled = roll(SOURCE, "0.92.0", "2026-09-23").unwrap();
    let (release, landing) = claim_changelogs(SOURCE, &rolled, "0.92.0", "2026-09-24").unwrap();
    assert_eq!(release, roll(SOURCE, "0.92.0", "2026-09-24").unwrap());
    assert_eq!(landing, rolled, "main is never rolled twice");
    let (release, landing) = claim_changelogs(&rolled, &rolled, "0.92.0", "2026-09-24").unwrap();
    assert_eq!(
        (release.as_str(), landing.as_str()),
        (rolled.as_str(), rolled.as_str())
    );
    claim_changelogs(&rolled, SOURCE, "0.92.0", "2026-09-24").unwrap_err();
}

/// The release body carries the changelog VERBATIM below the rule — the "used
/// once, verbatim" contract (spec §3) is about the notes themselves, and the
/// preamble must never edit them.
#[test]
fn the_release_body_is_preamble_then_the_changelog_verbatim() {
    let body = "### Fixed\n- a thing\n- another";
    let doc = release_notes_document("0.44.0", body);
    assert!(doc.ends_with(&format!("\n---\n\n{body}\n")), "{doc}");
}

/// NEGATIVE CONTROL: non-bracketed `## ` headings end a section consistently across
/// `real_body`, `rolled_body`, and `gate_section`, and suffix collisions on `## [0.1.0]extra`
/// do not match `0.1.0`.
#[test]
fn section_headers_are_exact_and_stop_at_any_h2() {
    let doc = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n\
               <!-- a -->- **Kept** — real note.\n\n\
               ## [0.1.0]extra\n\n- wrong\n\n\
               ## [0.1.0] - 2026-10-01\n\n### Fixed\n\n- shipped\n\n\
               ## Appendix\n\n'''\n";
    assert_eq!(
        real_body(doc, "Unreleased"),
        vec!["- **Kept** — real note."]
    );
    assert_eq!(gate_unreleased(doc).unwrap().entries, 1);
    assert!(has_section(doc, "0.1.0"));
    assert!(!has_section(doc, "0.2.0"));
    assert_eq!(rolled_body(doc, "0.1.0").unwrap(), "### Fixed\n\n- shipped");
    assert!(gate_section(doc, "0.1.0").is_ok());
}
