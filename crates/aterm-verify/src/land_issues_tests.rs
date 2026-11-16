// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OPEN-ISSUES RULE ([`issues`]): a merge whose union of the open-issues file's lines kept an
//! entry one side closed, put one entry's heading over another's text, or filed one entry's
//! lines under another's heading, lands the entry-level merge instead. Each test of such a merge
//! shows Git's own merge making it, and its control, where it has one, shows the rule keeps what
//! was opened or changed on purpose; the rest show a fix kept through later commits and merges,
//! and histories the rule must not stop.

use super::markdown::markdown_world;
use super::push::{narrow, resolve, round_green, stopped_on_conflict};
use super::*;
use aterm_census::known_issues::INDEX;

const HEAD: &str = "# Known issues\n\nOpen issues.\n\n";
const A: &str = "## ATERM-KI-1114-1 — a\n\nA's text.\n";
const B: &str = "## ATERM-KI-1114-2 — b\n\nB's text.\n";
const C: &str = "## ATERM-KI-1114-3 — c\n\nC's text.\n";
const D: &str = "## ATERM-KI-1114-4 — d\n\nD's text, new beside B.\n";
const E: &str = "## ATERM-KI-1114-5 — e\n\nE's text, opened by the branch.\n";
const F: &str = "## ATERM-KI-1114-6 — f\n\nF's text, opened by main.\n";
const B_HEADING: &str = "## ATERM-KI-1114-2 — b";
const F_HEADING: &str = "## ATERM-KI-1114-6 — f";

/// `entry` with `line` added after its text.
fn measured(entry: &str, line: &str) -> String {
    let (heading, text) = entry.split_once("\n\n").expect("a heading and a text");
    format!("{heading}\n\n{}{line}\n", text)
}

/// The phrase the rule's lines name the merge of `main` into `into` by.
fn merge_of(main: &str, into: &str) -> String {
    format!("the merge of {} into {}", short(main), short(into))
}

/// The open-issues file of `entries`.
fn issues(entries: &[&str]) -> String {
    format!("{HEAD}{}", entries.join("\n"))
}

/// [`markdown_world`] whose main also holds the open-issues file of A, B and C, merged as a
/// union of both sides' lines, as the live tree's `.gitattributes` merges it.
fn issues_world() -> World {
    let file = issues(&[A, B, C]);
    // Built from INDEX, so no string here spells the file's name: a file that does is one that
    // may read it, and every landing that shares it judges its crate again.
    let attributes = format!("{INDEX} merge=union\n");
    markdown_world(&[
        (INDEX, Some(&file), false),
        (".gitattributes", Some(&attributes), false),
    ])
}

/// The open-issues file at `at` in `world`'s work checkout, as Git shows it.
fn issues_at(world: &World, at: &str) -> String {
    git(&world.work, &["show", &format!("{at}:{INDEX}")])
}

/// The open-issues file Git's own merge of `main` into `into` makes in `world`, `into`'s lines
/// first, as `git merge` run on `into` makes it.
fn gits_merge(world: &World, main: &str, into: &str) -> String {
    let tree = git(
        &world.work,
        &["merge-tree", "--write-tree", "--no-messages", into, main],
    );
    issues_at(world, tree.lines().next().expect("a tree"))
}

/// The line the rule says, and notes, for B dropped from the merge of `main` into `into`, which
/// `deleted_by` closed.
fn dropped_line(main: &str, into: &str, deleted_by: &str) -> String {
    format!(
        "{INDEX}: ATERM-KI-1114-2 is dropped: {} deleted it, and the merge of {} into {} kept it",
        short(deleted_by),
        short(main),
        short(into)
    )
}

/// THE LANDING'S MERGE DROPS WHAT IT BROUGHT BACK: main closed B while the branch added D beside
/// it, so Git's merge of the two keeps B; the landing's merge holds D and not B, says so and
/// notes it after its message, and the judgment judges that tree. CONTROL: a branch that changed
/// B beside D opened it on purpose, and its change lands.
#[test]
fn the_landings_merge_drops_an_entry_it_brought_back_that_main_closed() {
    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    let feature = commit(&world.work, INDEX, &issues(&[A, D, B, C]));
    let main = rival_moves(&world, INDEX, &issues(&[A, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    assert!(
        gits_merge(&world, &main, &feature).contains(B_HEADING),
        "Git's merge brings B back"
    );
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| {
        let judged = std::fs::read_to_string(world.work.join(INDEX)).expect("the file");
        assert_eq!(judged, issues(&[A, D, C]), "the judgment judges the drop");
        green()
    });
    assert_eq!((code, bases.len()), (exit::PASS, 1), "{said:#?}");
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^@")])
            .lines()
            .collect::<Vec<_>>(),
        [feature.as_str(), main.as_str()]
    );
    assert_eq!(issues_at(&world, &landing), issues(&[A, D, C]).trim_end());
    let line = dropped_line(&main, &feature, &main);
    assert!(said.contains(&format!("verify land: {line}")), "{said:#?}");
    let message = git(&world.work, &["log", "-1", "--format=%B", &landing]);
    assert!(
        message.starts_with(&format!(
            "Merge origin/main into feature\n\n{}\n- ",
            issues::NOTE_HEAD
        )) && message.contains(&line),
        "{message}"
    );

    let control = issues_world();
    commit(&control.work, "a/x.txt", "a2\n");
    let changed = B.replace("B's text.", "B's text, changed.");
    commit(&control.work, INDEX, &issues(&[A, D, &changed, C]));
    rival_moves(&control, INDEX, &issues(&[A, C]));
    let (code, said, _) = land_with(&control, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landing = origin_main(&control);
    assert!(
        issues_at(&control, &landing).contains("B's text, changed."),
        "{said:#?}"
    );
    assert!(!said.iter().any(|line| line.contains(INDEX)), "{said:#?}");
}

/// A MERGE OF MAIN THE BRANCH MADE ITSELF IS HELD TO THE RULE: the branch closed B and merged a
/// main that added D beside it, which brought B back; the branch holds main's tip, so the
/// landing merges nothing, and the drop is a commit of its own ([`AUTOFIX_SUBJECT`], noted).
/// CONTROLS: where a later commit of the branch changed B, B stays open as it changed it; and
/// where main closed B, the branch's merge brought it back, and main then opened B again, B
/// stays open.
#[test]
fn a_merge_the_branch_made_is_held_to_the_rule_unless_b_was_opened_again_since() {
    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    let closed = commit(&world.work, INDEX, &issues(&[A, C]));
    let main = rival_moves(&world, INDEX, &issues(&[A, D, B, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let merge = git(&world.work, &["rev-parse", "HEAD"]);
    assert!(
        issues_at(&world, &merge).contains(B_HEADING),
        "Git's merge brings B back"
    );
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landing = origin_main(&world);
    assert_eq!(issues_at(&world, &landing), issues(&[A, D, C]).trim_end());
    let fix = git(&world.work, &["rev-parse", &format!("{landing}^1")]);
    assert_eq!(git(&world.work, &["rev-parse", &format!("{fix}^")]), merge);
    let line = dropped_line(&main, &closed, &closed);
    assert_eq!(
        git(&world.work, &["log", "-1", "--format=%B", &fix]),
        format!("{AUTOFIX_SUBJECT}\n\n{}\n- {line}", issues::NOTE_HEAD)
    );

    let edited = issues_world();
    commit(&edited.work, "a/x.txt", "a2\n");
    commit(&edited.work, INDEX, &issues(&[A, C]));
    rival_moves(&edited, INDEX, &issues(&[A, D, B, C]));
    git(&edited.work, &["fetch", "-q", "origin"]);
    git(&edited.work, &["merge", "-q", "--no-edit", "origin/main"]);
    assert!(
        issues_at(&edited, "HEAD").contains(B_HEADING),
        "Git's merge brings B back"
    );
    let changed = B.replace("B's text.", "B's text, changed since.");
    commit(&edited.work, INDEX, &issues(&[A, D, &changed, C]));
    let (code, said, _) = land_with(&edited, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        issues_at(&edited, &origin_main(&edited)),
        issues(&[A, D, &changed, C]).trim_end()
    );
    assert!(!said.iter().any(|line| line.contains(INDEX)), "{said:#?}");

    let control = issues_world();
    commit(&control.work, "a/x.txt", "a2\n");
    commit(&control.work, INDEX, &issues(&[A, D, B, C]));
    rival_moves(&control, INDEX, &issues(&[A, C]));
    git(&control.work, &["fetch", "-q", "origin"]);
    git(&control.work, &["merge", "-q", "--no-edit", "origin/main"]);
    assert!(
        issues_at(&control, "HEAD").contains(B_HEADING),
        "Git's merge brings B back"
    );
    rival_moves(&control, INDEX, &issues(&[A, B, C]));
    let (code, said, _) = land_with(&control, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landing = issues_at(&control, &origin_main(&control));
    assert_eq!(landing.matches(B_HEADING).count(), 1, "{landing}");
    assert!(!said.iter().any(|line| line.contains(INDEX)), "{said:#?}");
}

/// A FIX OF THE BRANCH'S OWN MERGE IS KEPT THROUGH ITS LATER COMMITS: the branch closed B and
/// merged a main that added D beside it, which brought B back, and then committed a file of
/// another kind, or opened E; the landing holds D and E, and not B, and says the line said of
/// the branch's merge.
#[test]
fn a_fix_of_the_branchs_own_merge_is_kept_through_its_later_commits() {
    for later in [None, Some(E)] {
        let world = issues_world();
        commit(&world.work, "a/x.txt", "a2\n");
        let closed = commit(&world.work, INDEX, &issues(&[A, C]));
        let main = rival_moves(&world, INDEX, &issues(&[A, D, B, C]));
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
        assert!(
            issues_at(&world, "HEAD").contains(B_HEADING),
            "Git's merge brings B back"
        );
        let expected = match later {
            None => {
                commit(&world.work, "a/y.txt", "a3\n");
                issues(&[A, D, C])
            }
            Some(entry) => {
                commit(&world.work, INDEX, &issues(&[A, D, B, C, entry]));
                issues(&[A, D, C, entry])
            }
        };
        let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
        assert_eq!(code, exit::PASS, "{later:?}: {said:#?}");
        let landing = origin_main(&world);
        assert_eq!(
            issues_at(&world, &landing),
            expected.trim_end(),
            "{later:?}"
        );
        let line = dropped_line(&main, &closed, &closed);
        assert!(
            said.contains(&format!("verify land: {line}")),
            "{later:?}: {said:#?}"
        );
    }
}

/// A LATER MERGE REPLAYS THE BRANCH'S OWN MERGE: the branch's merge of main brought back B,
/// which one side had closed (the branch, beside main's D; or main, beside the branch's D), and
/// main then moved elsewhere; the landing's merge of main's tip holds D and not B, and says the
/// line said of the branch's merge.
#[test]
fn a_later_merge_replays_the_branchs_own_merge() {
    for main_closes in [false, true] {
        let world = issues_world();
        commit(&world.work, "a/x.txt", "a2\n");
        let (branch, rival) = if main_closes {
            (issues(&[A, D, B, C]), issues(&[A, C]))
        } else {
            (issues(&[A, C]), issues(&[A, D, B, C]))
        };
        let feature = commit(&world.work, INDEX, &branch);
        let main = rival_moves(&world, INDEX, &rival);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
        let merge = git(&world.work, &["rev-parse", "HEAD"]);
        assert!(
            issues_at(&world, &merge).contains(B_HEADING),
            "Git's merge brings B back"
        );
        let tip = rival_moves(&world, "notes.txt", "main moved\n");
        git(&world.work, &["fetch", "-q", "origin"]);
        assert!(
            gits_merge(&world, &tip, &merge).contains(B_HEADING),
            "{main_closes}: Git's merge of main's tip keeps B"
        );
        let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
        assert_eq!(code, exit::PASS, "{main_closes}: {said:#?}");
        let landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{landing}^@")])
                .lines()
                .collect::<Vec<_>>(),
            [merge.as_str(), tip.as_str()]
        );
        assert_eq!(
            issues_at(&world, &landing),
            issues(&[A, D, C]).trim_end(),
            "{main_closes}"
        );
        let deleted_by = if main_closes { &main } else { &feature };
        let line = dropped_line(&main, &feature, deleted_by);
        assert!(
            said.contains(&format!("verify land: {line}")),
            "{main_closes}: {said:#?}"
        );
    }
}

/// A STACKED BRANCH'S MERGE BASES KEEP MAIN'S LATER CLOSURE: the child merged the old main
/// before its lower branch landed, so both lower tips are bases of the final merge. Main then
/// closed B beside the child's E; Git's union brings B back, and the landing must drop it.
/// CONTROLS: the child deliberately changes B, or main opens it again, and B stays open.
#[test]
fn a_stacked_branchs_multiple_merge_bases_preserve_closures_and_deliberate_reopenings() {
    for reopened in ["neither", "child", "main"] {
        let world = issues_world();
        commit(&world.work, "a/x.txt", "a2\n");
        let alpha = commit(&world.work, INDEX, &issues(&[A, D, B, C]));
        git(&world.work, &["branch", "stacked"]);
        let old_main = rival_moves(&world, "notes.txt", "main moved before alpha landed\n");
        git(&world.work, &["fetch", "-q", "origin"]);

        git(&world.work, &["checkout", "-q", "stacked"]);
        commit(&world.work, INDEX, &issues(&[A, D, E, B, C]));
        git(&world.work, &["merge", "-q", "--no-edit", &old_main]);
        assert_eq!(
            issues_at(&world, "HEAD"),
            issues(&[A, D, E, B, C]).trim_end(),
            "B was still open when the child merged main"
        );

        git(&world.work, &["checkout", "-q", "feature"]);
        let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
        assert_eq!(code, exit::PASS, "{reopened}: {said:#?}");
        let lower_landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{lower_landing}^@")])
                .lines()
                .collect::<Vec<_>>(),
            [alpha.as_str(), old_main.as_str()]
        );
        assert_eq!(
            issues_at(&world, &lower_landing),
            issues(&[A, D, B, C]).trim_end(),
            "the lower branch landed while B was open"
        );
        let closed = rival_moves(&world, INDEX, &issues(&[A, D, C]));

        git(&world.work, &["checkout", "-q", "stacked"]);
        let changed = measured(B, "Deliberately reopened by the child.");
        if reopened == "child" {
            commit(&world.work, INDEX, &issues(&[A, D, E, &changed, C]));
        }
        let main = if reopened == "main" {
            rival_moves(&world, INDEX, &issues(&[A, D, B, C]))
        } else {
            closed
        };
        let child = git(&world.work, &["rev-parse", "HEAD"]);
        git(&world.work, &["fetch", "-q", "origin"]);
        let bases: std::collections::BTreeSet<String> =
            git(&world.work, &["merge-base", "--all", &child, &main])
                .lines()
                .map(str::to_owned)
                .collect();
        assert_eq!(
            bases,
            std::collections::BTreeSet::from([alpha, old_main]),
            "{reopened}: both incomparable lower tips are merge bases"
        );
        assert!(
            gits_merge(&world, &main, &child).contains(B_HEADING),
            "{reopened}: Git's own merge holds B"
        );

        let expected = match reopened {
            "child" => issues(&[A, D, E, &changed, C]),
            "main" => issues(&[A, D, E, B, C]),
            _ => issues(&[A, D, E, C]),
        };
        let judged = std::cell::Cell::new(0);
        let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| {
            let now = std::fs::read_to_string(world.work.join(INDEX)).expect("the file judged");
            assert_eq!(now, expected, "{reopened}: the complete judged issues");
            judged.set(judged.get() + 1);
            green()
        });
        assert_eq!(
            (code, bases.len(), judged.get()),
            (exit::PASS, 1, 1),
            "{reopened}: {said:#?}"
        );
        assert_eq!(bases, [main], "the judgment is based on current main");
        let pushed = issues_at(&world, &origin_main(&world));
        assert_eq!(pushed, expected.trim_end(), "{reopened}");
        assert_eq!(
            pushed.matches(B_HEADING).count(),
            usize::from(reopened != "neither"),
            "{reopened}: a deliberate reopening is kept exactly once"
        );
    }
}

/// THREE BEST BASES KEEP EVERY SIDE'S ENTRIES: the child and main merge the same three lower
/// tips through different binary merges. Main later closes B, which the child's E brings back
/// in Git's union. The landing drops B and keeps D, E and the third side's F in full.
#[test]
fn three_merge_bases_keep_closed_issues_closed_and_each_sides_entries() {
    let world = issues_world();
    git(&world.work, &["checkout", "-q", "-b", "third"]);
    let third = commit(&world.work, INDEX, &issues(&[A, B, C, F]));
    git(&world.work, &["checkout", "-q", "feature"]);
    commit(&world.work, "a/x.txt", "a2\n");
    let alpha = commit(&world.work, INDEX, &issues(&[A, D, B, C]));
    git(&world.work, &["branch", "stacked"]);
    let old_main = rival_moves(&world, "notes.txt", "main moved before alpha landed\n");
    git(&world.work, &["fetch", "-q", "origin"]);

    git(&world.work, &["checkout", "-q", "stacked"]);
    commit(&world.work, INDEX, &issues(&[A, D, E, B, C]));
    git(&world.work, &["merge", "-q", "--no-edit", &old_main]);
    git(&world.work, &["merge", "-q", "--no-edit", &third]);
    let child = git(&world.work, &["rev-parse", "HEAD"]);
    assert_eq!(
        issues_at(&world, &child),
        issues(&[A, D, E, B, C, F]).trim_end()
    );

    git(&world.work, &["checkout", "-q", "feature"]);
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    git(&world.work, &["merge", "-q", "--no-edit", &third]);
    git(&world.work, &["push", "-q", "origin", "HEAD:main"]);
    assert_eq!(
        issues_at(&world, &origin_main(&world)),
        issues(&[A, D, B, C, F]).trim_end(),
        "all three lower sides reach main before B closes"
    );
    let main = rival_moves(&world, INDEX, &issues(&[A, D, C, F]));

    git(&world.work, &["checkout", "-q", "stacked"]);
    git(&world.work, &["fetch", "-q", "origin"]);
    let bases: std::collections::BTreeSet<String> =
        git(&world.work, &["merge-base", "--all", &child, &main])
            .lines()
            .map(str::to_owned)
            .collect();
    assert_eq!(
        bases,
        std::collections::BTreeSet::from([alpha, old_main, third])
    );
    assert!(gits_merge(&world, &main, &child).contains(B_HEADING));

    let expected = issues(&[A, D, E, C, F]);
    let judged = std::cell::Cell::new(0);
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| {
        assert_eq!(
            std::fs::read_to_string(world.work.join(INDEX)).expect("the file judged"),
            expected
        );
        judged.set(judged.get() + 1);
        green()
    });
    assert_eq!((code, judged.get()), (exit::PASS, 1), "{said:#?}");
    assert_eq!(bases, [main]);
    assert_eq!(issues_at(&world, &origin_main(&world)), expected.trim_end());
}

/// Commit an explicit issue-file resolution over two parents, without moving a ref.
fn issue_merge_commit(world: &World, tree: &str, first: &str, second: &str) -> String {
    git(
        &world.work,
        &[
            "commit-tree",
            tree,
            "-p",
            first,
            "-p",
            second,
            "-m",
            "explicit issue-file resolution",
        ],
    )
}

/// UNRELATED BEST BASES USE THE EMPTY ANCESTOR: two independent histories with the same files
/// are joined in opposite parent order. The child's bridge closes B while main's bridge adds
/// D beside it; Git's outer union resurrects B, and the replay must drop it while keeping D.
#[test]
fn unrelated_merge_bases_keep_a_bridge_closure_and_the_other_bridges_entries() {
    let world = issues_world();
    let one = git(&world.work, &["rev-parse", "HEAD"]);
    let tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    let other = git(
        &world.work,
        &["commit-tree", &tree, "-m", "an independent history"],
    );
    let bases = Git {
        root: &world.work,
        make: &git_cmd,
    }
    .complete(&["merge-base", "--all", &one, &other], LOCAL)
    .expect("the relation between the independent histories");
    assert_eq!(bases.status.code(), Some(1));
    assert!(bases.stdout.is_empty(), "the histories share no base");
    write(&world.work, INDEX, &issues(&[A, C]));
    git(&world.work, &["add", "--", INDEX]);
    let child_tree = git(&world.work, &["write-tree"]);
    let child = issue_merge_commit(&world, &child_tree, &one, &other);
    write(&world.work, INDEX, &issues(&[A, D, B, C]));
    git(&world.work, &["add", "--", INDEX]);
    let main_tree = git(&world.work, &["write-tree"]);
    let main = issue_merge_commit(&world, &main_tree, &other, &one);
    let bases: std::collections::BTreeSet<String> =
        git(&world.work, &["merge-base", "--all", &child, &main])
            .lines()
            .map(str::to_owned)
            .collect();
    assert_eq!(bases, std::collections::BTreeSet::from([one, other]));
    let outer = git(
        &world.work,
        &["merge-tree", "--write-tree", "--no-messages", &child, &main],
    );
    let outer = outer.lines().next().expect("the outer merge tree");
    assert!(issues_at(&world, outer).contains(B_HEADING));
    let (fixed, _) = issues::fixed(
        &Git {
            root: &world.work,
            make: &git_cmd,
        },
        &main,
        &child,
        Some(outer),
    )
    .expect("the unrelated internal bases can be merged")
    .expect("the outer union's resurrection is repaired");
    assert_eq!(fixed, issues(&[A, D, C]));
}

/// A NONREGULAR HISTORICAL BASE IS NOT AN EMPTY ISSUES FILE: both sides explicitly resolved it
/// to a regular readable file, but their virtual base is unsupported. The replay keeps the
/// original resolved merge and changes no source bytes, index tree or ref.
#[test]
fn a_nonregular_historical_base_keeps_the_original_merge_without_mutating_the_checkout() {
    let world = issues_world();
    let base = git(&world.work, &["rev-parse", "HEAD"]);
    let left = commit(&world.work, "a/x.txt", "a2\n");
    let blob = git(&world.work, &["rev-parse", &format!("HEAD:{INDEX}")]);
    git(
        &world.work,
        &["update-index", "--cacheinfo", "120000", &blob, INDEX],
    );
    let historical_tree = git(&world.work, &["write-tree"]);
    let right = git(
        &world.work,
        &[
            "commit-tree",
            &historical_tree,
            "-p",
            &base,
            "-m",
            "a historical nonregular issues entry",
        ],
    );
    assert!(git(&world.work, &["ls-tree", &right, "--", INDEX]).starts_with("120000 blob "));
    git(
        &world.work,
        &["update-index", "--cacheinfo", "100644", &blob, INDEX],
    );
    write(&world.work, INDEX, &issues(&[A, C]));
    git(&world.work, &["add", "--", INDEX]);
    let head_tree = git(&world.work, &["write-tree"]);
    let head = issue_merge_commit(&world, &head_tree, &left, &right);
    write(&world.work, INDEX, &issues(&[A, D, B, C]));
    git(&world.work, &["add", "--", INDEX]);
    let main_tree = git(&world.work, &["write-tree"]);
    let main = issue_merge_commit(&world, &main_tree, &right, &left);
    let outer = issue_merge_commit(&world, &main_tree, &head, &main);
    for at in [&head, &main, &outer] {
        assert!(git(&world.work, &["ls-tree", at, "--", INDEX]).starts_with("100644 blob "));
    }
    let bases: std::collections::BTreeSet<String> =
        git(&world.work, &["merge-base", "--all", &head, &main])
            .lines()
            .map(str::to_owned)
            .collect();
    assert_eq!(bases, std::collections::BTreeSet::from([left, right]));
    let resolved = issues(&[A, D, B, C]);
    assert_eq!(issues_at(&world, &outer), resolved.trim_end());
    let snapshot = || {
        (
            std::fs::read(world.work.join(INDEX)).expect("the source bytes"),
            git(&world.work, &["write-tree"]),
            git(&world.work, &["show-ref"]),
        )
    };
    let before = snapshot();
    let repaired = issues::fixed(
        &Git {
            root: &world.work,
            make: &git_cmd,
        },
        &main,
        &outer,
        None,
    )
    .expect("unsupported history keeps the original fallback without an error");
    assert_eq!(repaired, None);
    assert_eq!(snapshot(), before);
    assert_eq!(issues_at(&world, &outer), resolved.trim_end());
}

/// AN ENTRY BOTH SIDES CHANGED KEEPS EACH SIDE'S LINES: where the branch measured B while main
/// closed it and opened F in its place, Git's merge lost B's heading and filed the branch's line
/// under F's; where both measured A while main closed B, it filed main's line under the B it
/// kept. The landing holds each side's lines in the entry they were written in (B as the branch
/// changed it, beside main's F; A with both lines), and says why of each entry it changed.
#[test]
fn an_entry_both_sides_changed_keeps_each_sides_lines() {
    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    let changed = measured(B, "Measured by the branch: 3 of 10.");
    let feature = commit(&world.work, INDEX, &issues(&[A, &changed, C]));
    let main = rival_moves(&world, INDEX, &issues(&[A, F, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    let gits = gits_merge(&world, &main, &feature);
    assert!(
        !gits.contains(B_HEADING) && gits.contains(&format!("{F_HEADING}\n\nB's text.\nMeasured")),
        "Git's merge files the branch's line under F: {gits}"
    );
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landing = origin_main(&world);
    assert_eq!(
        issues_at(&world, &landing),
        issues(&[A, &changed, F, C]).trim_end()
    );
    let merge = merge_of(&main, &feature);
    for line in [
        format!(
            "{INDEX}: ATERM-KI-1114-2 merges both sides' changes line by line: both changed it, \
             and {merge} lost its heading"
        ),
        format!(
            "{INDEX}: ATERM-KI-1114-6 is as {} holds it: {merge} held it as neither side does",
            short(&main)
        ),
    ] {
        assert!(said.contains(&format!("verify land: {line}")), "{said:#?}");
    }

    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    let ours = measured(A, "Measured by the branch: 3 of 10.");
    let theirs = measured(A, "Measured by main: 7 of 10.");
    let feature = commit(&world.work, INDEX, &issues(&[&ours, B, C]));
    let main = rival_moves(&world, INDEX, &issues(&[&theirs, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    let gits = gits_merge(&world, &main, &feature);
    assert!(
        gits.contains(&format!("{B_HEADING}\n\nB's text.\nMeasured by main")),
        "Git's merge files main's line under B: {gits}"
    );
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let both = measured(&ours, "Measured by main: 7 of 10.");
    assert_eq!(
        issues_at(&world, &origin_main(&world)),
        issues(&[&both, C]).trim_end()
    );
    let merge = merge_of(&main, &feature);
    for line in [
        format!(
            "{INDEX}: ATERM-KI-1114-1 merges both sides' changes line by line: both changed it, \
             and {merge} held it as neither side does"
        ),
        dropped_line(&main, &feature, &main),
    ] {
        assert!(said.contains(&format!("verify land: {line}")), "{said:#?}");
    }
}

/// Commit `text` as the open-issues file into HEAD of `world`'s work checkout, its parents kept,
/// as a person amends a merge: the new HEAD.
fn amend_issues(world: &World, text: &str) -> String {
    std::fs::write(world.work.join(INDEX), text).expect("the file");
    git(&world.work, &["commit", "-q", "--amend", "--no-edit", "-a"]);
    git(&world.work, &["rev-parse", "HEAD"])
}

/// The branch's own merge of a main that closed B, both sides having measured A, as Git's union
/// makes it (main's line filed under the B it kept), and the branch's and main's A.
fn misfiled(world: &World) -> (String, String, String) {
    commit(&world.work, "a/x.txt", "a2\n");
    let ours = measured(A, "Measured by the branch: 3 of 10.");
    let theirs = measured(A, "Measured by main: 7 of 10.");
    commit(&world.work, INDEX, &issues(&[&ours, B, C]));
    rival_moves(world, INDEX, &issues(&[&theirs, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let union = issues_at(world, "HEAD");
    assert!(
        union.contains(&format!("{B_HEADING}\n\nB's text.\nMeasured by main")),
        "Git's merge files main's line under B: {union}"
    );
    (union, ours, theirs)
}

/// AN EDIT OF A PART THE UNION MISFILED KEEPS BOTH SIDES' LINES: both sides measured A while main
/// closed B, the branch's own merge filed main's line under the B Git's union kept, and then a
/// later commit of the branch, or the person amending that merge, added a line to A as that merge
/// holds it; the landing holds A with the branch's line, main's and the new one, and no B, where
/// taking the edited A whole would lose main's line with the B the rule drops.
#[test]
fn an_edit_of_a_part_the_union_misfiled_keeps_both_sides_lines() {
    for amend in [false, true] {
        let world = issues_world();
        let (union, ours, _) = misfiled(&world);
        let noted = measured(&ours, "Noted since: rerun on m21.");
        let edited = union.replacen(ours.trim_end(), noted.trim_end(), 1);
        assert_ne!(edited, union, "the edit is of A as the merge holds it");
        if amend {
            amend_issues(&world, &format!("{edited}\n"));
        } else {
            commit(&world.work, INDEX, &format!("{edited}\n"));
        }
        let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
        assert_eq!(code, exit::PASS, "{amend}: {said:#?}");
        let both = measured(&ours, "Measured by main: 7 of 10.");
        assert_eq!(
            issues_at(&world, &origin_main(&world)),
            issues(&[&measured(&both, "Noted since: rerun on m21."), C]).trim_end(),
            "{amend}"
        );
    }
}

/// E's heading as the branch opened it, and as Git's union leaves it bare above F ([`replaced`]).
const E_BARE: &str = "## ATERM-KI-1114-5 — e\n## ATERM-KI-1114-6 — f";

/// The branch's own merge of a main that opened F above B while the branch closed B and opened E
/// in its place, as Git's union makes it: E's heading bare above F, and B's heading over E's text.
fn replaced(world: &World) -> String {
    commit(&world.work, "a/x.txt", "a2\n");
    commit(&world.work, INDEX, &issues(&[A, E, C]));
    rival_moves(world, INDEX, &issues(&[A, F, B, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let union = issues_at(world, "HEAD");
    assert!(
        union.contains(E_BARE) && union.contains(&format!("{B_HEADING}\n\nE's text")),
        "Git's merge leaves E bare and puts B's heading over E's text: {union}"
    );
    union
}

/// Commit `text` as the open-issues file in `world`'s work checkout: into HEAD where `amend`, as a
/// person amends a merge ([`amend_issues`]), else as a later commit.
fn edit_issues(world: &World, amend: bool, text: &str) {
    if amend {
        amend_issues(world, text);
    } else {
        commit(&world.work, INDEX, text);
    }
}

/// The open-issues file the landing put on main in `world`, after a green landing of every crate.
fn landed(world: &World, what: &str) -> String {
    let (code, said, _) = land_with(world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{what}: {said:#?}");
    issues_at(world, &origin_main(world))
}

/// A BARE HEADING A PERSON DELETED KEEPS ITS ENTRY: the branch closed B and opened E in its place
/// while main opened F above B, and the branch's own merge left E's heading bare and put B's
/// heading over E's text ([`replaced`]); the person deleted the bare heading in that merge,
/// keeping E's text. The landing holds E with its text, F, and no B.
#[test]
fn a_bare_heading_a_person_deleted_keeps_its_entry() {
    let world = issues_world();
    let union = replaced(&world);
    amend_issues(
        &world,
        &format!("{}\n", union.replacen(E_BARE, F_HEADING, 1)),
    );
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        issues_at(&world, &origin_main(&world)),
        issues(&[A, E, F, C]).trim_end()
    );
}

/// The branch's own merge of a main that closed B and opened F in its place while the branch
/// measured B, as Git's union makes it (B's heading lost, its lines filed under F), and the
/// branch's B.
fn lost_heading(world: &World) -> (String, String) {
    commit(&world.work, "a/x.txt", "a2\n");
    let changed = measured(B, "Measured by the branch: 3 of 10.");
    commit(&world.work, INDEX, &issues(&[A, &changed, C]));
    rival_moves(world, INDEX, &issues(&[A, F, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let union = issues_at(world, "HEAD");
    assert!(
        !union.contains(B_HEADING) && union.contains(&format!("{F_HEADING}\n\nB's text.")),
        "Git's merge files B's lines under F: {union}"
    );
    (union, changed)
}

/// A PART PUT BACK STAYS OUT WHERE ITS LINES WERE TAKEN OUT: the branch's own merge lost B's
/// heading (main closed B, which the branch measured, and opened F), and then a later commit, or
/// the person amending that merge, took main's file; the rule would put B back with the branch's
/// line, but the edit took out B's lines with the heading, so the landing holds main's file.
#[test]
fn a_part_put_back_stays_out_where_its_lines_were_taken_out() {
    for amend in [false, true] {
        let world = issues_world();
        lost_heading(&world);
        let mains = issues(&[A, F, C]);
        if amend {
            amend_issues(&world, &mains);
        } else {
            commit(&world.work, INDEX, &mains);
        }
        let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
        assert_eq!(code, exit::PASS, "{amend}: {said:#?}");
        assert_eq!(
            issues_at(&world, &origin_main(&world)),
            mains.trim_end(),
            "{amend}"
        );
        let rules = format!("verify land: {INDEX}: ");
        assert!(
            !said.iter().any(|line| line.starts_with(&rules)),
            "{amend}: {said:#?}"
        );
    }
}

/// AN EDIT THAT TOOK OUT A LOST ENTRY'S LINES KEEPS THE RULE'S OTHER FIXES: the branch's own merge
/// both kept B, which main closed beside the branch's D, and lost the heading of H, which the
/// branch measured while main closed it and opened F in its place, filing H's lines under F; a
/// later commit took H's lines out of F. The landing holds D, F as main wrote it, and neither B
/// nor H: H stays out by itself, and B is dropped all the same, where refusing the whole step
/// would keep it.
#[test]
fn an_edit_that_took_out_a_lost_entrys_lines_keeps_the_rules_other_fixes() {
    let g = "## ATERM-KI-1114-7 — g\n\nG's text.\n";
    let h = "## ATERM-KI-1114-8 — h\n\nH's text.\n";
    let world = markdown_world(&[
        (INDEX, Some(&issues(&[A, B, C, g, h])), false),
        (
            ".gitattributes",
            Some(&format!("{INDEX} merge=union\n")),
            false,
        ),
    ]);
    commit(&world.work, "a/x.txt", "a2\n");
    let measured_h = measured(h, "Measured by the branch: 3 of 10.");
    commit(&world.work, INDEX, &issues(&[A, D, B, C, g, &measured_h]));
    rival_moves(&world, INDEX, &issues(&[A, C, g, F]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let union = issues_at(&world, "HEAD");
    let misfiled = "H's text.\nMeasured by the branch: 3 of 10.\n";
    assert!(
        union.contains(B_HEADING)
            && !union.contains("## ATERM-KI-1114-8")
            && union.contains(&format!("{F_HEADING}\n\n{misfiled}")),
        "Git's merge keeps B and files H's lines under F: {union}"
    );
    commit(
        &world.work,
        INDEX,
        &format!("{}\n", union.replacen(misfiled, "", 1)),
    );
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        issues_at(&world, &origin_main(&world)),
        issues(&[A, D, C, g, F]).trim_end()
    );
}

/// A PART PUT BACK IS HELD ONCE BESIDE A LATER EDIT OF ITS NEIGHBOUR: the branch's own merge lost
/// B's heading and filed B's lines under F, and a later commit measured F as that merge holds it;
/// the landing holds B, with the branch's line, once, and F with main's text and the new line,
/// where taking the edited F whole would hold B's lines twice.
#[test]
fn a_part_put_back_is_held_once_beside_a_later_edit_of_its_neighbour() {
    let world = issues_world();
    let (union, changed) = lost_heading(&world);
    let fs_text = "F's text, opened by main.";
    let line = "Measured F by the branch: 1 of 2.";
    assert_eq!(union.matches(fs_text).count(), 1, "{union}");
    commit(
        &world.work,
        INDEX,
        &format!(
            "{}\n",
            union.replacen(fs_text, &format!("{fs_text}\n{line}"), 1)
        ),
    );
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        issues_at(&world, &origin_main(&world)),
        issues(&[A, &changed, &measured(F, line), C]).trim_end()
    );
}

/// A LANDING WHOSE ONLY FIX IS THE RULE'S COMMITS IT: the branch's only change closed B, beside
/// main's D, so Git's merge holds main's tree whole, the closed B among it; the landing's merge
/// is amended to hold D and not B.
#[test]
fn a_landing_whose_only_fix_is_the_rules_commits_it() {
    let world = issues_world();
    let feature = commit(&world.work, INDEX, &issues(&[A, C]));
    let main = rival_moves(&world, INDEX, &issues(&[A, D, B, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    assert_eq!(
        gits_merge(&world, &main, &feature),
        issues(&[A, D, B, C]).trim_end(),
        "Git's merge holds main's file whole"
    );
    let (code, said, _) = land_with(&world, crates(&[]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^@")])
            .lines()
            .collect::<Vec<_>>(),
        [feature.as_str(), main.as_str()]
    );
    assert_eq!(issues_at(&world, &landing), issues(&[A, D, C]).trim_end());
    let line = dropped_line(&main, &feature, &feature);
    assert!(said.contains(&format!("verify land: {line}")), "{said:#?}");
}

/// A MERGE THAT PUT ONE ENTRY'S HEADING OVER ANOTHER'S TEXT LANDS THE ENTRY-LEVEL MERGE: the
/// branch closed B and opened E in its place while main opened F above B, and Git's merge lines
/// up E's lines with B's, so it leaves E's heading bare above F and puts B's heading over E's
/// text. The landing's merge holds E as the branch does, F as main does and no B, and says why
/// of each it changed. CONTROL: where main opened F at the end, Git's merge is the entry-level
/// one, and the landing lands it as Git made it, saying nothing.
#[test]
fn a_merge_that_put_one_entrys_heading_over_anothers_text_lands_the_entry_level_merge() {
    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    let feature = commit(&world.work, INDEX, &issues(&[A, E, C]));
    let main = rival_moves(&world, INDEX, &issues(&[A, F, B, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    let gits = gits_merge(&world, &main, &feature);
    assert!(
        gits.contains("## ATERM-KI-1114-5 — e\n## ATERM-KI-1114-6 — f")
            && gits.contains(&format!("{B_HEADING}\n\nE's text")),
        "Git's merge puts B's heading over E's text: {gits}"
    );
    let (code, said, bases) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!((code, bases.len()), (exit::PASS, 1), "{said:#?}");
    let landing = origin_main(&world);
    assert_eq!(
        issues_at(&world, &landing),
        issues(&[A, E, F, C]).trim_end()
    );
    let merge = format!("the merge of {} into {}", short(&main), short(&feature));
    for line in [
        format!(
            "{INDEX}: ATERM-KI-1114-2 is dropped: {} deleted it, and {merge} kept it",
            short(&feature)
        ),
        format!(
            "{INDEX}: ATERM-KI-1114-5 is as {} holds it: {merge} held it as neither side does",
            short(&feature)
        ),
    ] {
        assert!(said.contains(&format!("verify land: {line}")), "{said:#?}");
    }

    let control = issues_world();
    commit(&control.work, "a/x.txt", "a2\n");
    let feature = commit(&control.work, INDEX, &issues(&[A, E, C]));
    let main = rival_moves(&control, INDEX, &issues(&[A, B, C, F]));
    git(&control.work, &["fetch", "-q", "origin"]);
    let gits = gits_merge(&control, &main, &feature);
    assert_eq!(gits, issues(&[A, E, C, F]).trim_end());
    let (code, said, _) = land_with(&control, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(issues_at(&control, &origin_main(&control)), gits);
    assert!(!said.iter().any(|line| line.contains(INDEX)), "{said:#?}");
}

/// Resume `handoff` in `world`'s work checkout as this image, every judgment green: the exit,
/// what it said, and the open-issues file each judgment judged.
fn resume_handed(world: &World, handoff: Handoff) -> (i32, Vec<String>, Vec<String>) {
    let said = RefCell::new(Vec::new());
    let judged = RefCell::new(Vec::new());
    let code = resume(
        &world.work,
        &own(),
        handoff,
        &mut Seams {
            git: &git_cmd,
            verifier: &mut |_: &Path| Ok(own_image()),
            judge: &mut |root: &Path, _: &Path, _: &str, _: Poll<'_>| {
                let file = std::fs::read_to_string(root.join(INDEX)).expect("the file");
                judged.borrow_mut().push(file);
                Ran::Ended(green())
            },
            records: super::quiet(),
            reach: &|_: &Path, _: &str, _: Option<&[String]>| crates(&["a"]),
            recheck: &mut |_: &Path, _: &Path, base: &str, _: &Delta| {
                panic!("no round was expected (base {base})")
            },
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| said.borrow_mut().push(line.to_owned()),
            remote: &remote_step,
            pause: &no_retry,
        },
    );
    (code, said.into_inner(), judged.into_inner())
}

/// A JUDGMENT HANDED ON BY AN IMAGE WITHOUT THE RULE HOLDS ITS MERGE TO IT (ATERM-KI-1114-995):
/// the image that made the landing's merge (here `git merge`, as an image older than the rule
/// makes it) kept B, which main closed beside the branch's D, and handed the judgment on; the
/// image it was handed to amends the merge, its parents kept, and judges and lands D without B.
/// CONTROL: a handed merge the rule leaves as it is (main opened F at the end) is judged and
/// landed with the tree it was handed with, the rule saying nothing.
#[test]
fn a_judgment_handed_on_holds_its_merge_to_the_rule() {
    for closes in [true, false] {
        let world = issues_world();
        commit(&world.work, "a/x.txt", "a2\n");
        let feature = commit(&world.work, INDEX, &issues(&[A, D, B, C]));
        let theirs = if closes {
            issues(&[A, C])
        } else {
            issues(&[A, B, C, F])
        };
        let main = rival_moves(&world, INDEX, &theirs);
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
        let handed = git(&world.work, &["rev-parse", "HEAD"]);
        assert!(
            issues_at(&world, &handed).contains(B_HEADING),
            "Git's merge holds B"
        );
        let handoff = Handoff {
            root: world.work.clone(),
            image: OWN.into(),
            words: Vec::new(),
            step: aterm_land::Step::Qualify,
            hop: 1,
            at: handed.clone(),
            base: main.clone(),
            composes: 0,
            cone: main.clone(),
            branch: None,
            stated: Stated::default(),
            run: None,
        };
        let (code, said, judged) = resume_handed(&world, handoff);
        assert_eq!(code, exit::PASS, "{closes}: {said:#?}");
        let landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{landing}^@")])
                .lines()
                .collect::<Vec<_>>(),
            [feature.as_str(), main.as_str()],
            "{closes}"
        );
        let rules = format!("verify land: {INDEX}: ");
        if closes {
            let fixed = issues(&[A, D, C]);
            assert_eq!(judged, std::slice::from_ref(&fixed), "{said:#?}");
            assert_eq!(issues_at(&world, &landing), fixed.trim_end());
            let line = dropped_line(&main, &feature, &main);
            assert!(said.contains(&format!("verify land: {line}")), "{said:#?}");
            let message = git(&world.work, &["log", "-1", "--format=%B", &landing]);
            assert!(message.contains(&line), "{message}");
        } else {
            let tree = |at: &str| git(&world.work, &["rev-parse", &format!("{at}^{{tree}}")]);
            assert_eq!(tree(&landing), tree(&handed), "{said:#?}");
            assert!(
                !said.iter().any(|line| line.starts_with(&rules)),
                "{said:#?}"
            );
        }
    }
}

/// A MERGE OF HISTORIES THAT SHARE NO COMMIT IS NO ERROR: the branch merged a root commit of its
/// own, which has no merge base with the branch, and the landing replays it as a commit of its
/// first parent and lands.
#[test]
fn a_merge_of_histories_that_share_no_commit_lands() {
    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    let tree = git(&world.work, &["rev-parse", "HEAD:a"]);
    let root = git(
        &world.work,
        &["commit-tree", &tree, "-m", "a history of its own"],
    );
    git(
        &world.work,
        &[
            "merge",
            "-q",
            "--no-edit",
            "--allow-unrelated-histories",
            &root,
        ],
    );
    rival_moves(&world, INDEX, &issues(&[A, C]));
    let (code, said, _) = land_with(&world, crates(&["a"]), |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(
        issues_at(&world, &origin_main(&world)),
        issues(&[A, C]).trim_end()
    );
}

/// A ROUND WHOSE MERGE DROPS AN ENTRY IS STILL A ROUND: main closes B beside the branch's D
/// while the judgment runs, so Git's merge of the two keeps B. The merge the landing makes drops
/// it, which is the tree the round expects ([`Git::landing_tree`]), so the round judges it as
/// Git's own merge ([`Delta::mechanical`]) and nothing is judged again; the landing lacks B.
/// CONTROL: where the branch opened E at the end and main opens F there, the round expects the
/// checkout merge's order, the branch's lines first, and is a round, the rule saying nothing.
#[test]
fn a_round_whose_merge_drops_an_entry_is_still_a_round() {
    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    let judged = commit(&world.work, INDEX, &issues(&[A, D, B, C]));
    let moved = RefCell::new(String::new());
    let mechanical = RefCell::new(Vec::new());
    let branch = crates(&["a"]);
    let (code, said, bases, rechecks) = land_reaching(
        &world,
        &whole_reach(&branch),
        |_, n, _| {
            if n == 0 {
                *moved.borrow_mut() = rival_moves(&world, INDEX, &issues(&[A, C]));
            }
            Ran::Ended(green())
        },
        |_, delta| {
            mechanical.borrow_mut().push(delta.mechanical);
            super::markdown::rechecked()
        },
    );
    let moved = moved.into_inner();
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "rounded, not judged again: {said:#?}");
    assert_eq!(rechecks.len(), 1, "{said:#?}");
    assert_eq!(mechanical.into_inner(), [true], "{said:#?}");
    assert!(
        gits_merge(&world, &moved, &judged).contains(B_HEADING),
        "Git's merge brings B back"
    );
    let merge = &rechecks[0].1;
    assert_eq!(issues_at(&world, merge), issues(&[A, D, C]).trim_end());
    let landing = origin_main(&world);
    assert_eq!(issues_at(&world, &landing), issues(&[A, D, C]).trim_end());
    assert!(
        said.contains(&format!(
            "verify land: {}",
            dropped_line(&moved, &judged, &moved)
        )),
        "{said:#?}"
    );

    let control = issues_world();
    commit(&control.work, "a/x.txt", "a2\n");
    let judged = commit(&control.work, INDEX, &issues(&[A, B, C, E]));
    let moved = RefCell::new(String::new());
    let mechanical = RefCell::new(Vec::new());
    let (code, said, bases, rechecks) = land_reaching(
        &control,
        &whole_reach(&branch),
        |_, n, _| {
            if n == 0 {
                *moved.borrow_mut() = rival_moves(&control, INDEX, &issues(&[A, B, C, F]));
            }
            Ran::Ended(green())
        },
        |_, delta| {
            mechanical.borrow_mut().push(delta.mechanical);
            super::markdown::rechecked()
        },
    );
    let moved = moved.into_inner();
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "rounded, not judged again: {said:#?}");
    assert_eq!(mechanical.into_inner(), [true], "{said:#?}");
    assert_eq!(rechecks.len(), 1, "{said:#?}");
    // A union of two entries opened at the end glues them, which no rule here changes.
    let checkout_first = format!("{}{F}", issues(&[A, B, C, E]));
    assert_eq!(
        gits_merge(&control, &moved, &judged),
        checkout_first.trim_end()
    );
    assert_eq!(
        gits_merge(&control, &judged, &moved),
        format!("{}{E}", issues(&[A, B, C, F])).trim_end(),
        "with main's lines first, Git's merge is not the checkout's"
    );
    assert_eq!(
        issues_at(&control, &origin_main(&control)),
        checkout_first.trim_end()
    );
    let rules = format!("verify land: {INDEX}: ");
    assert!(
        !said.iter().any(|line| line.starts_with(&rules)),
        "{said:#?}"
    );
}

/// A MERGE A PERSON COMMITS AFTER A CONFLICT IS HELD TO THE RULE: main's conflicting move also
/// closed B beside the branch's D, the person resolved the conflict and committed Git's merge
/// of the rest, B among it; the next run amends that merge, its parents kept, and a round judges
/// it. CONTROLS: with no entry brought back the person's merge is judged as they committed it;
/// and an entry the person changed in the merge, B or D, is theirs and stands as they wrote it.
#[test]
fn a_merge_a_person_commits_after_a_conflict_is_held_to_the_rule() {
    for (closes, edits) in [
        (true, None),
        (false, None),
        (true, Some(B)),
        (true, Some(D)),
    ] {
        let world = issues_world();
        commit(&world.work, "a/x.txt", "a2\n");
        commit(&world.work, "shared.txt", &SHARED.replacen('4', "mine", 1));
        commit(&world.work, INDEX, &issues(&[A, D, B, C]));
        let (judged, _, tip) = stopped_on_conflict(&world, true, &|rival| {
            commit(rival, "shared.txt", &SHARED.replacen('4', "theirs", 1));
            if closes {
                commit(rival, INDEX, &issues(&[A, C]));
            }
        });
        let both = SHARED.replacen('4', "both", 1);
        let theirs = edits.map(|entry| {
            let merged = std::fs::read_to_string(world.work.join(INDEX)).expect("the file");
            let edited = entry.replace("text", "text, as the person wrote it");
            (
                merged.replacen(entry.trim_end(), edited.trim_end(), 1),
                edited,
            )
        });
        let mut files = vec![("shared.txt", both.as_str())];
        if let Some((file, _)) = &theirs {
            files.push((INDEX, file.as_str()));
        }
        let person = resolve(&world, &files);
        assert!(
            issues_at(&world, &person).contains(B_HEADING),
            "B is in the person's merge"
        );
        let (code, said, bases, rounds) = land_reaching(
            &world,
            &narrow,
            |_, _, _| panic!("a merge of a kept judgment is no judgment"),
            |_, _| round_green(),
        );
        assert_eq!(code, exit::PASS, "{closes} {edits:?}: {said:#?}");
        assert!(bases.is_empty(), "{closes} {edits:?}: {said:#?}");
        assert_eq!(rounds.len(), 1, "{closes} {edits:?}: {said:#?}");
        let landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{landing}^@")])
                .lines()
                .collect::<Vec<_>>(),
            [judged.as_str(), tip.as_str()],
            "{closes}: the landing is the person's merge, restated"
        );
        let landed = issues_at(&world, &landing);
        if let Some((_, edited)) = &theirs {
            assert!(landed.contains(edited.trim_end()), "{closes}: {landed}");
            // The person's B stands; D, which they left as Git made it, is the branch's, and B,
            // which they left as Git's union kept it, is dropped as main closed it.
            let expected = if edited.contains("B's") {
                issues(&[A, D, edited, C])
            } else {
                issues(&[A, edited, C])
            };
            assert_eq!(landed, expected.trim_end(), "{edits:?}");
        } else if closes {
            assert_eq!(landed, issues(&[A, D, C]).trim_end());
            assert_ne!(rounds[0].1, person, "the merge was amended");
            let message = git(&world.work, &["log", "-1", "--format=%B", &rounds[0].1]);
            assert!(
                message.contains(&dropped_line(&tip, &judged, &tip)),
                "{message}"
            );
        } else {
            assert_eq!(landed, issues(&[A, D, B, C]).trim_end());
            assert_eq!(rounds[0].1, person, "the person's merge, as committed");
        }
    }
}

/// A LINE WRITTEN INTO A CLOSED ENTRY GOES WITH THE TEXT IT FOLLOWS: in the branch's own merge
/// that put B's heading over E's text ([`replaced`]), a later commit, or the person amending the
/// merge, added a line after E's text; the rule closes B and puts the line in E, after E's text.
/// CONTROL: where the branch closed B and opened E while main measured B, Git's union lost B's
/// heading and filed B's lines under E's; a line added there after main's stays in E, the entry
/// it was written under, and B holds its lines as main does.
#[test]
fn a_line_written_into_a_closed_entry_goes_with_the_text_it_follows() {
    let text = "E's text, opened by the branch.\n";
    let note = "Measured since: 3 of 10.";
    for amend in [false, true] {
        let world = issues_world();
        let union = replaced(&world);
        edit_issues(
            &world,
            amend,
            &format!("{}\n", union.replacen(text, &format!("{text}{note}\n"), 1)),
        );
        assert_eq!(
            landed(&world, &format!("amend {amend}")),
            issues(&[A, &measured(E, note), F, C]).trim_end(),
            "{amend}"
        );
    }
    for amend in [false, true] {
        let world = issues_world();
        commit(&world.work, "a/x.txt", "a2\n");
        commit(&world.work, INDEX, &issues(&[A, E, C]));
        let main_b = measured(B, "Measured by main: 7 of 10.");
        rival_moves(&world, INDEX, &issues(&[A, &main_b, C]));
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
        let union = issues_at(&world, "HEAD");
        let held = "B's text.\nMeasured by main: 7 of 10.\n";
        assert!(
            !union.contains(B_HEADING) && union.contains(&format!("{text}{held}")),
            "Git's merge files B's lines under E: {union}"
        );
        let noted = "Noted on E since: rerun on m21.";
        edit_issues(
            &world,
            amend,
            &format!("{}\n", union.replacen(held, &format!("{held}{noted}\n"), 1)),
        );
        assert_eq!(
            landed(&world, &format!("control, amend {amend}")),
            issues(&[A, &main_b, &measured(E, noted), C]).trim_end(),
            "{amend}"
        );
    }
}

/// A CHANGE OF A LINE THE RULE MOVED IS MADE WHERE IT MOVED IT: in the branch's own merge that
/// lost B's heading and filed B's lines under F ([`lost_heading`]), the person amending the merge
/// updated the branch's measure of B where it stood; the landing holds B with the new measure,
/// and F with its own text alone.
#[test]
fn a_change_of_a_line_the_rule_moved_is_made_where_it_moved_it() {
    let world = issues_world();
    let (union, _) = lost_heading(&world);
    let (was, now) = (
        "Measured by the branch: 3 of 10.",
        "Measured by the branch: 4 of 10.",
    );
    amend_issues(&world, &format!("{}\n", union.replacen(was, now, 1)));
    assert_eq!(
        landed(&world, "the measure"),
        issues(&[A, &measured(B, now), F, C]).trim_end()
    );
}

/// AN ENTRY THE UNION LEFT A BARE HEADING STAYS CLOSED WHERE A LATER COMMIT TOOK IT OUT: the
/// branch retitled B while main closed it and opened F in its place, and the branch's own merge
/// held B's new heading bare (main's deletion of B's text merged clean); a later commit deleted
/// that heading, or took main's file. The rule would put B back whole, but the edit took out all
/// the merge held of it: the landing holds A, F and C.
#[test]
fn an_entry_the_union_left_a_bare_heading_stays_closed_where_a_later_commit_took_it_out() {
    let retitled = "## ATERM-KI-1114-2 — b, partly fixed";
    for take_main in [false, true] {
        let world = issues_world();
        commit(&world.work, "a/x.txt", "a2\n");
        commit(
            &world.work,
            INDEX,
            &issues(&[A, &B.replacen(B_HEADING, retitled, 1), C]),
        );
        rival_moves(&world, INDEX, &issues(&[A, F, C]));
        git(&world.work, &["fetch", "-q", "origin"]);
        git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
        let union = issues_at(&world, "HEAD");
        assert!(
            union.contains(&format!("{retitled}\n{F_HEADING}")) && !union.contains("B's text"),
            "Git's merge holds B's new heading bare: {union}"
        );
        let text = if take_main {
            issues(&[A, F, C])
        } else {
            format!("{}\n", union.replacen(&format!("{retitled}\n"), "", 1))
        };
        commit(&world.work, INDEX, &text);
        assert_eq!(
            landed(&world, &format!("take main {take_main}")),
            issues(&[A, F, C]).trim_end(),
            "{take_main}"
        );
    }
}

/// A PERSON WHO DELETES THE CLOSED ENTRY GIT BROUGHT BACK KEEPS THE LINES IT HELD OF ANOTHER:
/// both sides measured A while main closed B, and the branch's own merge filed main's line
/// under the B Git's union kept ([`misfiled`]); the person amending that merge deleted B's block,
/// main's line with it. The landing holds A with both sides' lines. And where a later commit
/// instead closed A (deleted A's block, as its fix does), the landing holds C alone: main's line
/// of A goes with A.
#[test]
fn a_person_who_deletes_a_closed_entry_keeps_the_lines_it_held_of_another() {
    let world = issues_world();
    let (union, ours, theirs) = misfiled(&world);
    let (b, c) = (union.find(B_HEADING), union.find("## ATERM-KI-1114-3 "));
    let (Some(b), Some(c)) = (b, c) else {
        panic!("B and C in the merge: {union}")
    };
    amend_issues(&world, &format!("{}{}\n", &union[..b], &union[c..]));
    let both = measured(
        &ours,
        theirs.trim_end().rsplit('\n').next().expect("main's line"),
    );
    assert_eq!(landed(&world, "B deleted"), issues(&[&both, C]).trim_end());

    let world = issues_world();
    let (union, _, _) = misfiled(&world);
    let (a, b) = (union.find("## ATERM-KI-1114-1 "), union.find(B_HEADING));
    let (Some(a), Some(b)) = (a, b) else {
        panic!("A and B in the merge: {union}")
    };
    commit(
        &world.work,
        INDEX,
        &format!("{}{}\n", &union[..a], &union[b..]),
    );
    assert_eq!(landed(&world, "A closed"), issues(&[C]).trim_end());
}

/// A RETITLE OF A BARE HEADING KEEPS ITS TEXT: in the branch's own merge that left E's heading
/// bare and put B's heading over E's text ([`replaced`]), the person amending the merge retitled
/// E; the landing holds E under its new heading with its text, F, and no B.
#[test]
fn a_retitle_of_a_bare_heading_keeps_its_text() {
    let world = issues_world();
    let union = replaced(&world);
    let retitled = "## ATERM-KI-1114-5 — e, narrowed to the GPU path";
    amend_issues(
        &world,
        &format!(
            "{}\n",
            union.replacen("## ATERM-KI-1114-5 — e\n", &format!("{retitled}\n"), 1)
        ),
    );
    assert_eq!(
        landed(&world, "retitled"),
        issues(&[A, &E.replacen("## ATERM-KI-1114-5 — e", retitled, 1), F, C]).trim_end()
    );
}

/// A HEADING A LATER COMMIT DELETED KEEPS ITS ENTRY BESIDE ONE THE RULE CLOSED: main closed B
/// beside the branch's D, Git's union kept B, and a later commit deleted C's heading, so C's
/// text sits under B; the rule closes B and C keeps its text, where taking the later commit's B
/// would put the closed B back holding C's text.
#[test]
fn a_heading_a_later_commit_deleted_keeps_its_entry_beside_one_the_rule_closed() {
    let world = issues_world();
    commit(&world.work, "a/x.txt", "a2\n");
    commit(&world.work, INDEX, &issues(&[A, D, B, C]));
    rival_moves(&world, INDEX, &issues(&[A, C]));
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--no-edit", "origin/main"]);
    let union = issues_at(&world, "HEAD");
    assert!(union.contains(B_HEADING), "Git's merge keeps B: {union}");
    commit(
        &world.work,
        INDEX,
        &format!("{}\n", union.replacen("## ATERM-KI-1114-3 — c\n\n", "", 1)),
    );
    assert_eq!(landed(&world, "C's heading"), issues(&[A, D, C]).trim_end());
}

/// A STEP WHOSE MERGE LINE BY LINE FAILS THE LINE COUNT IS MADE PART BY PART
/// ([`issues::reapply`]): the repair closed 1, and the edit took out 2 (its heading and one line)
/// and wrote 2's other line into 1; neither the merged 1 (which the edit only moved a line into)
/// nor 2 (whose line the edit took out) stands, which would lose that line, so the step holds 1
/// as the edit does, and that result passes the count.
#[test]
fn a_step_whose_line_merge_fails_the_count_is_made_part_by_part() {
    let one = |text: &str| format!("## ATERM-KI-1114-1 — t1\n\n{text}");
    let two = |text: &str| format!("## ATERM-KI-1114-2 — t2\n\n{text}");
    let (was, repaired, edited) = (
        issues(&[&one("x2.\n"), &two("x4.\nx0.\n")]),
        issues(&[&two("x4.\nx0.\n")]),
        issues(&[&one("x2.\nx0.\n")]),
    );
    assert!(
        !issues::accounts(&was, &repaired, &edited, &issues(&[])),
        "neither part standing loses x0"
    );
    let now = issues::reapply(&was, &repaired, &edited).expect("a text the parser reads");
    assert_eq!(now, edited);
    assert!(issues::accounts(&was, &repaired, &edited, &now));
}

/// THE LINE GUARD ([`issues::accounts`]): a step's result holds each line of a part's text the
/// edit holds, less what the repair took out of a part and more what it put in one, and no
/// other count: one that loses a line of the edit's, or holds one more often, fails it. A line
/// the repair moved from one part to another may be held where either holds it, or not at all
/// where the edit took it out of the part it went to. A heading is the part's identifier and no
/// line of its text.
#[test]
fn the_rules_result_answers_for_every_line_of_the_edit() {
    let (was, repaired, edited) = ("a\nb\n", "a\n", "a\nb\nc\n");
    for (result, holds) in [
        ("a\nc\n", true),
        ("a\nb\nc\n", true),
        ("a\n", false),
        ("a\nc\nc\n", false),
        ("a\nc\nd\n", false),
    ] {
        assert_eq!(
            issues::accounts(was, repaired, edited, result),
            holds,
            "{result:?}"
        );
    }
    for (result, holds) in [("a\nd\n", true), ("a\n", true), ("a\nd\nd\n", false)] {
        assert_eq!(
            issues::accounts("a\n", "a\nd\n", "a\n", result),
            holds,
            "{result:?}"
        );
    }
    assert!(
        issues::accounts("a\n\n\nb  \n", "a\n", "a\nb\n", "a\n\nb\n"),
        "blank lines and white space at a line's end are not counted"
    );
    let (bare, held) = (issues(&[A, "## ATERM-KI-1114-5 — e\n", C]), issues(&[A, C]));
    assert!(
        issues::accounts(&bare, &issues(&[A, E, C]), &held, &issues(&[A, E, C])),
        "a heading is counted as the part's identifier, not as a line of its text"
    );
    assert!(
        !issues::accounts(&held, &held, &held, &issues(&[A, B, C])),
        "a line of a part's text no side holds is made up"
    );
    let note = "Measured by main: 7 of 10.";
    let (misfiled, moved) = (
        issues(&[A, &measured(B, note)]),
        issues(&[&measured(A, note)]),
    );
    for (result, holds) in [
        (issues(&[&measured(A, note)]), true),
        (issues(&[A]), true),
        (issues(&[&measured(&measured(A, note), note)]), true),
        (
            issues(&[&measured(&measured(&measured(A, note), note), note)]),
            false,
        ),
    ] {
        assert_eq!(
            issues::accounts(&misfiled, &moved, &misfiled, &result),
            holds,
            "a line the repair moved from B to A: {result}"
        );
    }
    assert!(
        !issues::accounts(&held, &held, &held, &issues(&[A, A])),
        "a text the parser does not read is refused"
    );
}

#[test]
fn open_issues_checkout_guard_accepts_the_regular_file() {
    let world = issues_world();
    let make = git_cmd;
    let git = Git {
        root: &world.work,
        make: &make,
    };
    git.writable(INDEX).expect("regular tracked issues file");
}

#[cfg(unix)]
#[test]
fn open_issues_checkout_guard_refuses_a_link_without_touching_its_target() {
    let world = issues_world();
    let outside = world.work.join("guard-control.txt");
    std::fs::write(&outside, "kept").unwrap();
    let file = world.work.join(INDEX);
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(&outside, &file).unwrap();
    let git = Git {
        root: &world.work,
        make: &git_cmd,
    };
    assert!(git.writable(INDEX).unwrap_err().contains("symbolic link"));
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "kept");
}
