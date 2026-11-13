// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HEAD: &str = "# Changelog\n\n";

fn changelog(body: &str) -> String {
    format!("{HEAD}## [Unreleased]\n\n{body}")
}

fn occurrences(text: &str, needle: &str) -> usize {
    text.match_indices(needle).count()
}

#[test]
fn one_sided_main_entries_keep_their_authored_spacing() {
    let base = changelog("### Fixed\n\n- **Existing.** Shared.\n");
    let main = changelog("### Fixed\n\n- **New note.** Added on main.\n- **Existing.** Shared.\n");
    assert_eq!(merge(&base, &base, &main).unwrap(), main);
}

/// Main rewrote two entries of one subsection after the branch forked, and the branch never
/// touched the file. The same rewrite on the tip stays a conflict
/// (`ambiguity_and_competing_edits_name_the_entries`), and so does main's rewrite once the
/// tip writes an entry of its own: only a tip byte-identical to the base takes main's bytes.
#[test]
fn a_branch_that_left_the_file_takes_mains_ambiguous_rewrite() {
    let base = changelog("### Fixed\n\n- **Old A.** A.\n\n- **Old B.** B.\n");
    let main = changelog("### Fixed\n\n- **New A.** A.\n\n- **New B.** B.\n");
    assert_eq!(merge(&base, &base, &main).unwrap(), main);

    let tip = changelog(
        "### Fixed\n\n- **The branch's.** Its own.\n\n- **Old A.** A.\n\n- **Old B.** B.\n",
    );
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(
        conflict
            .reason
            .contains("main has an ambiguous multi-entry replacement"),
        "{conflict:?}"
    );
}

#[test]
fn the_real_0194_shape_rewrites_by_entry_and_collapses_reordered_kinds() {
    let base = changelog(
        "### Fixed\n\n- **Release qualification.** Stages privately.\n\n- **No machine is reserved.** A held number has an owner.\n\n### Changed\n\n- **Harness versions.** v0.31.\n\n- **An idle node wakes less often.** Its listener and sharing wait.\n\n### Fixed\n\n- **Commons idles.** It waits for work.\n",
    );
    let tip = changelog(
        "### Fixed\n\n- **Release qualification.** Stages privately.\n\n- **No machine is reserved.** A held number has an owner.\n\n- **Commons idles.** It waits for work.\n\n### Changed\n\n- **Harness versions.** v0.31.\n\n- **An idle node no longer polls.** Its sharing waits for a change.\n",
    );
    let main = changelog(
        "### Fixed\n\n- **Release qualification.** Stages privately.\n\n- **No machine is reserved.** A held number has an owner.\n\n### Changed\n\n- **Yolo is one-way.** A click only enables it.\n\n- **Harness versions.** v0.31.\n\n- **An idle node wakes less often.** Its listener and sharing wait.\n\n### Fixed\n\n- **Commons idles.** It waits for work.\n",
    );
    let merged = merge(&base, &tip, &main).unwrap();
    assert_eq!(occurrences(&merged, "### Fixed"), 1, "{merged}");
    assert_eq!(occurrences(&merged, "### Changed"), 1, "{merged}");
    assert!(
        !merged.contains("An idle node wakes less often"),
        "{merged}"
    );
    assert!(merged.contains("An idle node no longer polls"), "{merged}");
    assert!(merged.contains("Yolo is one-way"), "{merged}");
    assert!(
        merged.find("Commons idles").unwrap() < merged.find("### Changed").unwrap(),
        "{merged}"
    );
}

#[test]
fn a_rewrite_replaces_old_words_while_main_adds_nearby() {
    let base = changelog("### Fixed\n\n- **Old words.** One.\n");
    let tip = changelog("### Fixed\n\n- **Better words.** One, precisely.\n");
    let main =
        changelog("### Fixed\n\n- **Main's nearby fix.** Concurrent.\n\n- **Old words.** One.\n");
    let merged = merge(&base, &tip, &main).unwrap();
    assert!(!merged.contains("**Old words.**"), "{merged}");
    assert!(merged.contains("**Better words.**"), "{merged}");
    assert!(merged.contains("**Main's nearby fix.**"), "{merged}");
}

#[test]
fn a_main_release_is_exact_and_the_tip_addition_stays_unreleased() {
    let base = changelog("### Fixed\n\n- **Shipped.** In the next cut.\n");
    let tip = changelog(
        "### Fixed\n\n- **Branch addition.** Written before the cut.\n\n- **Shipped.** In the next cut.\n",
    );
    let release = "## [0.2.0] - 2026-11-10\n\n### Fixed\n\n- **Shipped.** In the next cut.\n";
    let main = format!("{HEAD}## [Unreleased]\n\n{release}");
    let merged = merge(&base, &tip, &main).unwrap();
    let at = merged.find("## [0.2.0]").unwrap();
    assert_eq!(&merged[at..], release);
    assert!(merged[..at].contains("Branch addition"), "{merged}");
    assert!(!merged[at..].contains("Branch addition"), "{merged}");
}

#[test]
fn independently_different_release_sections_are_a_named_conflict() {
    let base = changelog("");
    let tip = format!(
        "{HEAD}## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n### Fixed\n\n- **Branch release.** Independently written.\n"
    );
    let main = format!(
        "{HEAD}## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n### Fixed\n\n- **Main release.** Cut on main.\n"
    );
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("independently"), "{conflict:?}");
    assert_eq!(conflict.entries, ["Branch release.", "Main release."]);
}

#[test]
fn independently_different_release_numbers_are_a_named_conflict() {
    let base = changelog("### Fixed\n\n- **Before cut.** Shared entry.\n");
    let tip = format!(
        "{HEAD}## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n### Fixed\n\n- **Before cut.** Shared entry.\n"
    );
    let main = format!(
        "{HEAD}## [Unreleased]\n\n## [0.3.0] - 2026-11-10\n\n### Fixed\n\n- **Before cut.** Shared entry.\n"
    );
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("authoritative"), "{conflict:?}");
    assert_eq!(conflict.entries, ["Before cut."]);
}

#[test]
fn a_rewrite_while_main_cuts_keeps_the_replacement_unreleased() {
    let base = changelog("### Fixed\n\n- **Old words.** Before the cut.\n");
    let tip = changelog("### Fixed\n\n- **New words.** After review.\n");
    let main = format!(
        "{HEAD}## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n### Fixed\n\n- **Old words.** Before the cut.\n"
    );
    let merged = merge(&base, &tip, &main).unwrap();
    let cut = merged.find("## [0.2.0]").unwrap();
    assert!(merged[..cut].contains("**New words.**"), "{merged}");
    assert!(!merged[..cut].contains("**Old words.**"), "{merged}");
    assert_eq!(&merged[cut..], &main[main.find("## [0.2.0]").unwrap()..]);
}

#[test]
fn an_identical_independent_addition_main_shipped_is_not_published_twice() {
    let base = changelog("");
    let tip = changelog("### Fixed\n\n- **Same addition.** Independently written.\n");
    let main = format!(
        "{HEAD}## [Unreleased]\n\n## [0.2.0] - 2026-11-10\n\n### Fixed\n\n- **Same addition.** Independently written.\n"
    );
    let merged = merge(&base, &tip, &main).unwrap();
    assert_eq!(merged, main);
    assert_eq!(occurrences(&merged, "**Same addition.**"), 1);
}

#[test]
fn copying_an_existing_released_entry_requires_resolution() {
    let released = "## [0.1.0] - 2026-11-01\n\n### Fixed\n\n- **Already shipped.** Exact bytes.\n";
    let base = format!("{}{released}", changelog(""));
    let tip = format!(
        "{}{released}",
        changelog("### Fixed\n\n- **Already shipped.** Exact bytes.\n")
    );
    let main = format!(
        "{}{released}",
        changelog("### Fixed\n\n- **Main addition.** Concurrent.\n")
    );
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("already released"), "{conflict:?}");
    assert_eq!(conflict.entries, ["Already shipped."]);
}

#[test]
fn code_whitespace_is_byte_significant_entry_content() {
    let base = changelog("");
    let tip = changelog("### Fixed\n\n- `a  b`. Branch spacing.\n");
    let main = changelog("### Fixed\n\n- `a b`. Main spacing.\n");
    let merged = merge(&base, &tip, &main).unwrap();
    assert!(merged.contains("`a  b`"), "{merged}");
    assert!(merged.contains("`a b`"), "{merged}");
}

#[test]
fn a_hand_restored_entry_is_kept_from_the_final_three_way_state() {
    let base = changelog("### Fixed\n\n- **Restored by hand.** Keep me.\n");
    let tip = base.clone();
    let main = changelog(
        "### Fixed\n\n- **Main addition.** Keep this too.\n\n- **Restored by hand.** Keep me.\n",
    );
    let merged = merge(&base, &tip, &main).unwrap();
    assert_eq!(occurrences(&merged, "Restored by hand"), 1, "{merged}");
    assert!(merged.contains("Main addition"), "{merged}");
}

#[test]
fn one_sided_reorder_wins_and_mains_gap_addition_survives() {
    let base = changelog("### Fixed\n\n- **A.** First.\n\n- **B.** Second.\n\n- **C.** Third.\n");
    let tip = changelog("### Fixed\n\n- **C.** Third.\n\n- **A.** First.\n\n- **B.** Second.\n");
    let main = changelog(
        "### Fixed\n\n- **A.** First.\n\n- **M.** Main insertion.\n\n- **B.** Second.\n\n- **C.** Third.\n",
    );
    let merged = merge(&base, &tip, &main).unwrap();
    let positions =
        ["**C.**", "**A.**", "**M.**", "**B.**"].map(|entry| merged.find(entry).unwrap());
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{merged}"
    );
}

#[test]
fn a_shared_addition_at_different_anchors_is_a_named_conflict() {
    let base = changelog("### Fixed\n\n- **A.** First.\n\n- **B.** Second.\n");
    let tip = changelog(
        "### Fixed\n\n- **X.** Shared addition.\n\n- **A.** First.\n\n- **B.** Second.\n",
    );
    let main = changelog(
        "### Fixed\n\n- **A.** First.\n\n- **B.** Second.\n\n- **X.** Shared addition.\n",
    );
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(
        conflict.reason.contains("different anchors"),
        "{conflict:?}"
    );
    assert_eq!(conflict.entries, ["X."]);
}

#[test]
fn differently_worded_concurrent_additions_with_one_title_conflict() {
    let base = changelog("");
    let tip = changelog("### Fixed\n\n- **Same title.** Branch words.\n");
    let main = changelog("### Fixed\n\n- **Same title.** Main words.\n");
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("added"), "{conflict:?}");
    assert_eq!(conflict.entries, ["Same title."]);

    let converged = merge(&base, &tip, &tip).unwrap();
    assert_eq!(occurrences(&converged, "**Same title.**"), 1);
}

#[test]
fn moved_rewrite_versus_removal_is_a_named_conflict() {
    let base = changelog("### Fixed\n\n- **Stable title.** Old words.\n");
    let removed = changelog("");
    let moved = changelog("### Changed\n\n- **Stable title.** Rewritten and moved.\n");
    let conflict = merge(&base, &removed, &moved).unwrap_err();
    assert!(conflict.reason.contains("removed"), "{conflict:?}");
    assert_eq!(conflict.entries, ["Stable title."]);
}

#[test]
fn section_reorders_follow_three_way_order_or_conflict() {
    let section = |version: &str, title: &str| {
        format!("## [{version}]\n\n### Fixed\n\n- **{title}.** Entry.\n\n")
    };
    let unreleased = "# Changelog\n\n## [Unreleased]\n\n";
    let a = section("0.3.0", "A");
    let b = section("0.2.0", "B");
    let c = section("0.1.0", "C");
    let base = format!("{unreleased}{a}{b}{c}");
    let tip = format!("{unreleased}{b}{a}{c}");
    let merged = merge(&base, &tip, &base).unwrap();
    assert!(
        merged.find("## [0.2.0]") < merged.find("## [0.3.0]"),
        "{merged}"
    );

    let main = format!("{unreleased}{a}{c}{b}");
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("sections"), "{conflict:?}");
    assert_eq!(
        conflict.entries,
        ["## [0.1.0]", "## [0.2.0]", "## [0.3.0]", "## [Unreleased]",]
    );
}

#[test]
fn release_cut_and_new_section_anchors_cannot_be_silently_relocated() {
    let section = |version: &str, title: &str| {
        format!("## [{version}]\n\n### Fixed\n\n- **{title}.** Entry.\n\n")
    };
    let head = "# Changelog\n\n";
    let unreleased = section("Unreleased", "U");
    let a = section("0.2.0", "A");
    let b = section("0.1.0", "B");
    let release = section("0.3.0", "R");
    let base = format!("{head}{unreleased}{a}{b}");
    let tip = format!("{head}{a}{unreleased}{b}");
    let main = format!("{head}{unreleased}{release}{a}{b}");
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("anchors"), "{conflict:?}");
    assert_eq!(conflict.entries, ["## [0.3.0]"]);

    let shared_tip = format!("{head}{release}{unreleased}{a}{b}");
    let conflict = merge(&base, &shared_tip, &main).unwrap_err();
    assert!(conflict.reason.contains("placed"), "{conflict:?}");
    assert_eq!(conflict.entries, ["## [0.3.0]"]);
}

#[test]
fn new_subsection_anchors_cannot_be_silently_relocated() {
    const FIXED: &str = "### Fixed\n\n- **F.** Fixed.\n\n";
    const CHANGED: &str = "### Changed\n\n- **C.** Changed.\n\n";
    const ADDED: &str = "### Added\n\n- **N.** New.\n\n";
    let base = changelog(&format!("{FIXED}{CHANGED}"));
    let tip = changelog(&format!("{CHANGED}{FIXED}"));
    let main = changelog(&format!("{FIXED}{ADDED}{CHANGED}"));
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("anchors"), "{conflict:?}");
    assert_eq!(conflict.entries, ["### Added"]);

    let shared_tip = changelog(&format!("{ADDED}{FIXED}{CHANGED}"));
    let conflict = merge(&base, &shared_tip, &main).unwrap_err();
    assert!(conflict.reason.contains("placed"), "{conflict:?}");
    assert_eq!(conflict.entries, ["### Added"]);
}

#[test]
fn a_kind_kept_only_by_a_new_entry_uses_that_entries_anchors() {
    const A: &str = "### A\n\n- **Old A.** Base entry.\n\n";
    const B: &str = "### B\n\n- **Old B.** Removed by the branch.\n\n";
    const C: &str = "### C\n\n- **Old C.** Base entry.\n\n";
    const NEW_B: &str = "- **New B.** Added by main.\n\n";
    let base = changelog(&format!("{A}{B}{C}"));
    let tip = changelog(&format!("{C}{A}"));
    let main = changelog(&format!("{A}{B}{NEW_B}{C}"));
    let conflict = merge(&base, &tip, &main).unwrap_err();
    assert!(conflict.reason.contains("anchors"), "{conflict:?}");
    assert_eq!(conflict.entries, ["### B"]);
}

#[test]
fn ambiguity_and_competing_edits_name_the_entries() {
    let base = changelog("### Fixed\n\n- **Old A.** A.\n\n- **Old B.** B.\n");
    let tip = changelog("### Fixed\n\n- **New A.** A.\n\n- **New B.** B.\n");
    let conflict = merge(&base, &tip, &base).unwrap_err();
    assert!(conflict.reason.contains("ambiguous"), "{conflict:?}");
    assert_eq!(conflict.entries, ["New A.", "New B.", "Old A.", "Old B."]);

    let old = changelog("### Fixed\n\n- **Old.** Base.\n");
    let removed = changelog("");
    let rewritten = changelog("### Fixed\n\n- **New.** Tip.\n");
    let conflict = merge(&old, &removed, &rewritten).unwrap_err();
    assert!(conflict.reason.contains("removed"), "{conflict:?}");
    assert_eq!(conflict.entries, ["New.", "Old."]);

    let other = changelog("### Fixed\n\n- **Other.** Main.\n");
    let conflict = merge(&old, &rewritten, &other).unwrap_err();
    assert!(conflict.reason.contains("differently"), "{conflict:?}");
    assert_eq!(conflict.entries, ["New.", "Old.", "Other."]);
}

#[test]
fn non_entry_ambiguity_names_each_documents_own_entries() {
    let base = changelog("### Fixed\n\n- **Base title.** Original.\n");
    let tip = changelog(
        "Prose whose placement is not an entry.\n\n### Fixed\n\n- **Tip title.** Rewritten.\n",
    );
    let conflict = merge(&base, &tip, &base).unwrap_err();
    assert!(conflict.reason.contains("non-entry"), "{conflict:?}");
    assert_eq!(conflict.entries, ["Base title.", "Tip title."]);
}

#[test]
fn an_entry_without_a_subsection_keeps_its_order_group() {
    let base = changelog("");
    let tip = changelog("- **Bare entry.** Deliberately has no level-three heading.\n");
    let merged = merge(&base, &tip, &base).unwrap();
    assert!(merged.contains("**Bare entry.**"), "{merged}");
    assert!(!merged.contains("###"), "{merged}");
}

#[test]
fn removal_beats_unchanged_and_plain_history_is_byte_exact() {
    let plain = "## Earlier releases\n\nOld prose and links stay byte-for-byte.\n";
    let base = format!(
        "{}{}",
        changelog("### Fixed\n\n- **Gone.** Intentionally.\n"),
        plain
    );
    let removed = format!("{}{}", changelog(""), plain);
    let merged = merge(&base, &removed, &base).unwrap();
    assert!(!merged.contains("Gone"), "{merged}");
    assert!(merged.ends_with(plain), "{merged}");
}

#[test]
fn differing_titles_names_content_and_order_differences_only() {
    let a = changelog("### Fixed\n\n- **A.** One.\n\n- **B.** Two.\n");
    let b = changelog("### Fixed\n\n- **B.** Two.\n\n- **A.** One.\n");
    assert_eq!(differing_titles(&a, &b).unwrap(), ["A.", "B."]);
}

#[test]
fn the_live_changelog_is_accepted_byte_exact() {
    let path = aterm_tempfile::manifest_dir().join("../..").join(PATH);
    let live = match std::fs::read_to_string(&path) {
        Ok(live) => live,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("read {}: {error}", path.display()),
    };
    assert_eq!(merge(&live, &live, &live).unwrap(), live);
}
