// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_types::land_trailer::{Checked, Row};

/// Replay immutable release history without entering publication or touching its host.
#[test]
#[ignore = "measurement over the caller's repository history"]
fn replay_release_selection() {
    let root = aterm_tempfile::manifest_dir().join("../..");
    let base = std::env::var("ATERM_REPLAY_BASE").unwrap();
    let tip = std::env::var("ATERM_REPLAY_TIP").unwrap();
    let began = std::time::Instant::now();
    let input = discover(
        &root,
        &std::env::var_os("PATH").unwrap_or_default(),
        &base,
        &tip,
    )
    .unwrap();
    println!(
        "selection: {} commits, {} paths, {:.3}s",
        input.commits.len(),
        input.paths.len(),
        began.elapsed().as_secs_f64()
    );
    for commit in input.commits {
        println!("commit {commit}");
    }
    for path in input.paths {
        println!("path {path}");
    }
}

struct Repo(aterm_tempfile::TempDir);
impl Repo {
    fn new() -> Self {
        let repo = Self(aterm_tempfile::tempdir().unwrap());
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.name", "Fixture"]);
        repo.git(&["config", "user.email", "fixture@example.invalid"]);
        repo.write("seed", "seed\n");
        repo.commit("base");
        repo
    }
    fn git(&self, args: &[&str]) -> String {
        git(
            self.0.path(),
            &std::env::var_os("PATH").unwrap_or_default(),
            args,
        )
        .unwrap()
        .trim()
        .to_string()
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.path().join(name), text).unwrap();
    }
    fn commit(&self, message: &str) -> String {
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", message]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn input(&self, base: &str) -> Input {
        discover(
            self.0.path(),
            &std::env::var_os("PATH").unwrap_or_default(),
            base,
            "HEAD",
        )
        .unwrap()
    }
    fn receipt(&self, base: &str) -> String {
        Stated {
            checked: vec![Checked {
                tree: self.git(&["rev-parse", "HEAD^{tree}"]),
                base: base.into(),
                secs: 1,
                rows: NAMES
                    .into_iter()
                    .map(|name| Row {
                        check: name.into(),
                        word: "ok".into(),
                    })
                    .collect(),
            }],
            ..Stated::default()
        }
        .trailers()
    }
    fn landing(&self, tree: &str, branch: &str, main: &str, receipt: &Stated) -> String {
        let id = self.git(&[
            "commit-tree",
            tree,
            "-p",
            branch,
            "-p",
            main,
            "-m",
            &format!("land\n\n{}", receipt.trailers()),
        ]);
        self.git(&["update-ref", "HEAD", &id]);
        id
    }
}

#[test]
fn empty_input_needs_no_workspace_or_check_tools() {
    let repo = Repo::new();
    let head = repo.git(&["rev-parse", "HEAD"]);
    let mut log = Vec::new();
    assert!(judge(repo.0.path(), &head, &head, &CHECKS, None, &mut log).is_ok());
    assert!(log.is_empty(), "no source check was entered");
    assert!(
        Input {
            commits: vec![head],
            paths: vec!["seed".into()]
        }
        .no_work_note()
        .is_none()
    );
}

/// Lint is qualification's, not an early source reader's, and the CLI says so: a `--checks`
/// name outside the readers is a usage error that names it, the readers, and where lint
/// runs. NEGATIVE CONTROL: the CLI used to exit 2 with nothing on stderr, the explanation
/// living only in `judge`, which no CLI path reaches.
#[test]
fn early_readers_cannot_dispatch_qualification_lint() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    let error = judge(
        r.0.path(),
        &base,
        &base,
        &[Check::Lint],
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(!error.0);
    assert!(error.1.contains("qualification"));
    for (names, unknown) in [("lint", "lint"), ("fmt,lint", "lint"), ("bogus", "bogus")] {
        let mut err = Vec::new();
        assert_eq!(
            run_cli_to(
                &[
                    "--base".into(),
                    base.clone(),
                    "--checks".into(),
                    names.into()
                ],
                r.0.path(),
                &mut err,
            ),
            crate::exit::USAGE,
            "{names}"
        );
        let said = String::from_utf8(err).unwrap();
        assert!(said.contains(&format!("`{unknown}`")), "{names}: {said:?}");
        assert!(
            said.contains("export-scan, fmt, census"),
            "{names}: {said:?}"
        );
        assert_eq!(
            said.contains("verify release --tree --checks lint"),
            unknown == "lint",
            "{names}: {said:?}"
        );
    }
    // The readers themselves parse, in the release's order, with nothing to explain.
    assert_eq!(checks_named("fmt"), Ok(vec![Check::Fmt]));
    assert_eq!(
        checks_named("census,export-scan"),
        Ok(vec![Check::ExportScan, Check::Census])
    );
}

/// Each option is given at most once, and a `--checks` list names every check it wants:
/// a repeated option or an empty name is a usage error that says which, before anything is
/// read or opened. NEGATIVE CONTROL: a second `--checks` silently replaced the first (and
/// before that intersected with it), and an empty name was reported as an unknown reader
/// named by an empty pair of backticks.
#[test]
fn an_option_is_given_once_and_a_checks_list_names_every_check() {
    let said = |args: &[&str]| {
        let mut err = Vec::new();
        let mut argv = vec!["--base".to_owned(), "0".repeat(40)];
        argv.extend(args.iter().map(|&arg| arg.to_owned()));
        let exit = run_cli_to(&argv, Path::new("/nonexistent"), &mut err);
        (exit, String::from_utf8(err).unwrap())
    };
    for flag in [
        "--checks",
        "--events",
        "--carry-context",
        "--passed-input-items",
    ] {
        let (exit, why) = said(&[flag, "fmt", flag, "census", "--unknown", "x"]);
        assert_eq!(exit, crate::exit::USAGE, "{flag}: {why:?}");
        assert!(
            why.contains(&format!("`{flag}` is given twice")),
            "{flag}: {why:?}"
        );
    }
    for (list, words) in [
        ("", "--checks: the list names no check"),
        ("fmt,", "--checks: `fmt,` has an empty name"),
        (",census", "--checks: `,census` has an empty name"),
    ] {
        let (exit, why) = said(&["--checks", list]);
        assert_eq!(exit, crate::exit::USAGE, "{list:?}");
        assert!(why.contains(words), "{list:?}: {why:?}");
        assert!(!why.contains("``"), "{list:?}: {why:?}");
    }
}

/// The very same tree is owed without its real landing trailer, and covered with it.
/// An earlier unchecked main commit stays owed: the receipt's base is not its cone.
#[test]
fn hand_merge_and_landed_merge_have_different_input() {
    let r = Repo::new();
    let released = r.git(&["rev-parse", "HEAD"]);
    r.write("unreviewed", "unreviewed\n");
    let base = r.commit("unchecked main");
    r.git(&["checkout", "-qb", "topic"]);
    r.write("reviewed", "reviewed\n");
    let topic = r.commit("topic");
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let receipt = r.receipt(&base);
    let hand = r.git(&[
        "commit-tree",
        &tree,
        "-p",
        &topic,
        "-p",
        &base,
        "-m",
        "hand merge",
    ]);
    r.git(&["update-ref", "HEAD", &hand]);
    let before = r.input(&released);
    assert_eq!(before.paths, ["reviewed", "unreviewed"]);
    let landed = r.git(&[
        "commit-tree",
        &tree,
        "-p",
        &topic,
        "-p",
        &base,
        "-m",
        &format!("land\n\n{receipt}"),
    ]);
    r.git(&["update-ref", "HEAD", &landed]);
    let after = r.input(&released);
    assert_eq!(after.paths, ["unreviewed"]);
    assert!(!after.commits.contains(&topic));
    // A receipt for another tree cannot hide the hand merge's new content.
    let stale = receipt.replace(&tree, &"f".repeat(40));
    let bad = r.git(&[
        "commit-tree",
        &tree,
        "-p",
        &topic,
        "-p",
        &base,
        "-m",
        &format!("stale\n\n{stale}"),
    ]);
    r.git(&["update-ref", "HEAD", &bad]);
    assert_eq!(r.input(&released).paths, before.paths);
}

/// A checked edit to an earlier unchecked path covers its resulting entry, including
/// deletion. Subsequent content or mode changes are still owed.
#[test]
fn later_judgment_covers_entries_but_never_later_edits() {
    let r = Repo::new();
    let released = r.git(&["rev-parse", "HEAD"]);
    r.write("reviewed", "unchecked\n");
    r.write("deleted", "unchecked\n");
    r.write("other", "unchecked\n");
    let base = r.commit("unchecked main");
    r.write("reviewed", "checked\n");
    std::fs::remove_file(r.0.path().join("deleted")).unwrap();
    let topic = r.commit("repair and judge");
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.landing(&tree, &topic, &base, &receipt);
    assert_eq!(r.input(&released).paths, ["other"]);
    r.write("reviewed", "later unchecked\n");
    r.commit("later edit");
    assert_eq!(r.input(&released).paths, ["other", "reviewed"]);
    r.write("reviewed", "checked\n");
    r.commit("restore checked bytes");
    assert_eq!(r.input(&released).paths, ["other"]);
    r.git(&["update-index", "--chmod=+x", "reviewed"]);
    r.git(&["commit", "-qm", "unchecked mode"]);
    assert_eq!(r.input(&released).paths, ["other", "reviewed"]);
}

/// A merge's new resolution is its own input even when both parents were judged.
#[test]
fn merge_resolution_cannot_hide_behind_checked_parents() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    r.write("seed", "topic\n");
    let topic = r.commit("topic");
    r.git(&["checkout", "--detach", &base]);
    r.write("seed", "main\n");
    let main = r.commit("main");
    r.write("seed", "resolution\n");
    r.git(&["add", "."]);
    let tree = r.git(&["write-tree"]);
    let merged = r.git(&[
        "commit-tree",
        &tree,
        "-p",
        &topic,
        "-p",
        &main,
        "-m",
        "resolution",
    ]);
    r.git(&["update-ref", "HEAD", &merged]);
    assert_eq!(r.input(&base).paths, ["seed"]);
}

/// An octopus merge's input is every path where it differs from any parent, as
/// `--diff-merges=separate` reads it, gathered one parent at a time. Here both tips are
/// already released, so the archive merge alone is owed, with its new file and each path
/// where it differs from a tip.
///
/// NEGATIVE CONTROL: read the first parent's diff alone and the paths are `manifest` only;
/// read it as one `--diff-merges=separate` capture and a large enough archive merge
/// overflows the complete-capture limit, the refusal of 0.193.0's first dry run on main's
/// 60-parent preservation merge 7514a3dd04d.
#[test]
fn an_octopus_merge_owes_every_path_where_it_differs_from_a_parent() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    r.write("tip-a", "a\n");
    let a = r.commit("tip a");
    r.git(&["checkout", "-q", "--detach", &base]);
    r.write("tip-b", "b\n");
    let b = r.commit("tip b");
    r.git(&["merge", "-q", "--no-edit", &a]);
    let released = r.git(&["rev-parse", "HEAD"]);
    r.write("manifest", "kept\n");
    r.git(&["add", "."]);
    let tree = r.git(&["write-tree"]);
    let merged = r.git(&[
        "commit-tree",
        &tree,
        "-p",
        &released,
        "-p",
        &a,
        "-p",
        &b,
        "-m",
        "archive",
    ]);
    r.git(&["update-ref", "HEAD", &merged]);
    let input = r.input(&released);
    assert_eq!(input.commits, std::slice::from_ref(&merged));
    assert_eq!(input.paths, ["manifest", "tip-a", "tip-b"]);
    let separate = r.git(&[
        "show",
        "--format=",
        "--name-only",
        "--no-renames",
        "--diff-merges=separate",
        &merged,
    ]);
    let separate: BTreeSet<&str> = separate.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(separate, BTreeSet::from(["manifest", "tip-a", "tip-b"]));
}

/// Choosing a parent's old content can undo a checked fix. A combined diff omits that
/// path, but comparison with the automatic merge still finds the unjudged edit.
#[test]
fn a_resolution_equal_to_one_parent_is_still_unchecked() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    let old_tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    r.write("fix", "checked fix\n");
    let topic = r.commit("fix");
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let receipt = r.receipt(&base);
    let landed = r.git(&[
        "commit-tree",
        &tree,
        "-p",
        &topic,
        "-p",
        &base,
        "-m",
        &format!("land\n\n{receipt}"),
    ]);
    let evil = r.git(&[
        "commit-tree",
        &old_tree,
        "-p",
        &landed,
        "-p",
        &base,
        "-m",
        "drop the fix",
    ]);
    r.git(&["update-ref", "HEAD", &evil]);
    let input = r.input(&base);
    assert_eq!(input.commits, [evil]);
    assert_eq!(input.paths, ["fix"]);
}

#[test]
fn semantic_changelog_result_cannot_borrow_its_parents_judgment() {
    let r = Repo::new();
    let notes = "## [Unreleased]\n\n### Fixed\n\n- Old claim.\n";
    r.write("CHANGELOG.md", notes);
    let base = r.commit("base notes");
    r.write(
        "CHANGELOG.md",
        &notes.replace("Old claim.", "Corrected claim."),
    );
    let branch = r.commit("checked correction");
    let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.git(&["checkout", "--detach", &base]);
    r.write("CHANGELOG.md", &format!("{notes}\n- Main addition.\n"));
    let main = r.commit("main addition");
    // Combine both entry edits without judging the resulting tree.
    r.write(
        "CHANGELOG.md",
        &format!(
            "{}\n- Main addition.\n",
            notes.replace("Old claim.", "Corrected claim.")
        ),
    );
    r.git(&["add", "."]);
    let tree = r.git(&["write-tree"]);
    receipt.composed_onto = Some(main.clone());
    let landing = r.landing(&tree, &branch, &main, &receipt);
    let input = r.input(&base);
    assert!(input.commits.contains(&landing));
    assert_eq!(input.paths, ["CHANGELOG.md"]);
    // Actual judgment of this tree changes the verdict.
    receipt.checked[0].tree = tree.clone();
    r.landing(&tree, &branch, &main, &receipt);
    assert!(r.input(&base).paths.is_empty());
}

#[test]
fn historical_repeated_subsections_keep_exact_one_sided_composition_credit() {
    let r = Repo::new();
    let notes = "## [Unreleased]\n\n### Fixed\n\n- First.\n\n### Fixed\n\n- Second.\n";
    r.write("CHANGELOG.md", notes);
    let base = r.commit("old repeated subsections");
    r.write("branch", "checked\n");
    let branch = r.commit("branch");
    let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.git(&["checkout", "--detach", &base]);
    r.write("CHANGELOG.md", &notes.replace("Second.", "Main's change."));
    let main = r.commit("main");
    r.git(&["checkout", "--detach", &branch]);
    r.git(&["merge", "--no-ff", "-m", "historical merge", &main]);
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    receipt.composed_onto = Some(main.clone());
    let landing = r.landing(&tree, &branch, &main, &receipt);
    // Repeated headings preserve every entry. The branch left this file
    // untouched, so its exact native composition earns only branch credit;
    // main's new notes remain owed.
    let input = r.input(&base);
    assert!(!input.commits.contains(&landing));
    assert_eq!(input.commits.as_slice(), std::slice::from_ref(&main));
    assert_eq!(input.paths, ["CHANGELOG.md"]);
    // A forged resolution still owes checks even with the repeated headings.
    r.write("branch", "unjudged resolution\n");
    r.git(&["add", "."]);
    let bad_tree = r.git(&["write-tree"]);
    let bad = r.landing(&bad_tree, &branch, &main, &receipt);
    let input = r.input(&base);
    assert!(input.commits.contains(&bad));
    assert_eq!(input.paths, ["CHANGELOG.md", "branch"]);
}

/// A composed landing carries only the checked branch's delta. Main that arrived while
/// it ran remains owed, and an extra resolution edit invalidates the composition claim.
#[test]
fn composition_excludes_new_main_and_checks_its_actual_tree() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    r.write("branch", "checked\n");
    let branch = r.commit("branch");
    let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.git(&["checkout", "--detach", &base]);
    r.write("main", "not checked\n");
    let main = r.commit("main");
    r.git(&["checkout", "--detach", &branch]);
    r.git(&["merge", "--no-ff", "-m", "merge", &main]);
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    receipt.composed_onto = Some(main.clone());
    let landing = r.git(&[
        "commit-tree",
        &tree,
        "-p",
        &branch,
        "-p",
        &main,
        "-m",
        &format!("land\n\n{}", receipt.trailers()),
    ]);
    r.git(&["update-ref", "HEAD", &landing]);
    let input = r.input(&base);
    assert_eq!(input.commits.as_slice(), std::slice::from_ref(&main));
    assert_eq!(input.paths, ["main"]);
    r.write("branch", "unjudged resolution\n");
    r.git(&["add", "."]);
    let bad_tree = r.git(&["write-tree"]);
    let bad = r.git(&[
        "commit-tree",
        &bad_tree,
        "-p",
        &branch,
        "-p",
        &main,
        "-m",
        &format!("bad\n\n{}", receipt.trailers()),
    ]);
    r.git(&["update-ref", "HEAD", &bad]);
    assert!(r.input(&base).paths.contains(&"branch".into()));
}

/// A re-judge names the prior checked branch as its base, not the main ancestor
/// from which the later composition's disjoint files must be computed.
#[test]
fn composed_rejudgment_uses_the_parents_common_main() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    r.write("branch", "checked\n");
    let branch = r.commit("branch");
    let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.git(&["checkout", "--detach", &base]);
    r.write("main", "first\n");
    let main = r.commit("main");
    r.git(&["checkout", "--detach", &branch]);
    r.git(&["merge", "--no-ff", "-m", "rejudge", &main]);
    let rejudged = r.git(&["rev-parse", "HEAD"]);
    receipt
        .checked
        .extend(Stated::of_trailers(&r.receipt(&branch)).unwrap().checked);
    r.git(&["checkout", "--detach", &main]);
    r.write("main", "second\n");
    let moved = r.commit("moved main");
    r.git(&["checkout", "--detach", &rejudged]);
    r.git(&["merge", "--no-ff", "-m", "compose", &moved]);
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    receipt.composed_onto = Some(moved.clone());
    r.landing(&tree, &rejudged, &moved, &receipt);
    let input = r.input(&base);
    assert_eq!(input.commits.as_slice(), std::slice::from_ref(&moved));
    assert_eq!(input.paths, ["main"]);
    // A stated composition cannot authorize an extra resolution edit.
    r.write("branch", "forged\n");
    r.git(&["add", "."]);
    let bad = r.git(&["write-tree"]);
    r.landing(&bad, &rejudged, &moved, &receipt);
    assert!(r.input(&base).paths.contains(&"branch".into()));
}

/// An existing ancestor tree is no more authority than a nonexistent forged tree:
/// without a valid composition it cannot hide even that ancestor's branch delta.
#[test]
fn unrelated_tree_trailer_cannot_cover_a_branch_prefix() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    r.write("branch", "checked prefix\n");
    let branch = r.commit("branch");
    let receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.write("extra", "unchecked\n");
    r.commit("extra");
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let parent = r.git(&["rev-parse", "HEAD"]);
    r.landing(&tree, &parent, &base, &receipt);
    let input = r.input(&base);
    assert!(input.commits.contains(&branch));
    assert_eq!(input.paths, ["branch", "extra"]);
}

/// The release boundary truncates the in-memory graph, not the receipt's actual
/// ancestry. A base before that boundary is valid; an unrelated base is not.
#[test]
fn receipt_base_before_the_release_boundary_is_checked_in_git() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    r.write("released", "released\n");
    let released = r.commit("released");
    r.write("branch", "checked\n");
    let branch = r.commit("branch");
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.landing(&tree, &branch, &released, &receipt);
    assert!(r.input(&released).commits.is_empty());
    r.git(&["checkout", "--detach", &base]);
    r.write("other", "unrelated\n");
    let unrelated = r.commit("unrelated base");
    r.git(&["checkout", "--detach", &branch]);
    receipt.checked[0].base = unrelated;
    r.landing(&tree, &branch, &released, &receipt);
    assert!(r.input(&released).commits.contains(&branch));
}

/// Sixty commits, only two without evidence: history size does not widen the input.
#[test]
fn sixty_commit_cone_keeps_only_two_unchecked_changes() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    let began = std::time::Instant::now();
    for index in 0..29 {
        let parent = r.git(&["rev-parse", "HEAD"]);
        r.write("reviewed", &format!("{index}\n"));
        let topic = r.commit("topic");
        let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
        let message = format!("land\n\n{}", r.receipt(&parent));
        let landing = r.git(&[
            "commit-tree",
            &tree,
            "-p",
            &topic,
            "-p",
            &parent,
            "-m",
            &message,
        ]);
        r.git(&["update-ref", "HEAD", &landing]);
    }
    for name in ["one", "two"] {
        r.write(name, name);
        r.commit(name);
    }
    let start = std::time::Instant::now();
    let input = r.input(&base);
    assert_eq!(input.paths, ["one", "two"]);
    println!(
        "release-input fixture: setup_s={:.3} discovery_s={:.3} commits={} paths={}",
        began.elapsed().as_secs_f64(),
        start.elapsed().as_secs_f64(),
        input.commits.len(),
        input.paths.len()
    );
}

/// A Markdown compose's recheck (`Aterm-Rechecked`) is the landing's last judgment: it binds the
/// landing whose tree it names and credits the branch's delta from the main it merged, the
/// source readers having read the merge. Main's own commits since the judged base stay owed.
/// NEGATIVE CONTROLS: the same landing without the recheck line, or with one naming another
/// tree, binds nothing, so the branch's commits are owed too.
#[test]
fn a_recheck_binds_its_landing_and_leaves_mains_own_commits_owed() {
    let r = Repo::new();
    let base = r.git(&["rev-parse", "HEAD"]);
    r.git(&["checkout", "-qb", "topic"]);
    r.write("branch.md", "checked\n");
    let topic = r.commit("branch");
    let mut receipt = Stated::of_trailers(&r.receipt(&base)).unwrap();
    r.git(&["checkout", "--detach", &base]);
    r.write("main.md", "main's own\n");
    let main = r.commit("main");
    r.git(&["checkout", "--detach", &topic]);
    r.git(&["merge", "--no-ff", "-qm", "merge main", &main]);
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let without = receipt.clone();
    receipt
        .rechecked
        .push(aterm_types::land_trailer::Rechecked {
            tree: tree.clone(),
            rows: NAMES
                .into_iter()
                .map(|name| Row {
                    check: name.into(),
                    word: "ok".into(),
                })
                .collect(),
            base: main.clone(),
            onto: main.clone(),
            secs: 1,
            after: 1,
        });
    r.landing(&tree, &topic, &main, &receipt);
    let input = r.input(&base);
    assert_eq!(input.commits.as_slice(), std::slice::from_ref(&main));
    assert_eq!(input.paths, ["main.md"]);

    r.landing(&tree, &topic, &main, &without);
    let unbound = r.input(&base);
    assert!(unbound.commits.contains(&topic), "{unbound:?}");
    assert_eq!(unbound.paths, ["branch.md", "main.md"]);
    let mut elsewhere = receipt.clone();
    elsewhere.rechecked[0].tree = "f".repeat(40);
    r.landing(&tree, &topic, &main, &elsewhere);
    assert_eq!(r.input(&base).paths, unbound.paths);
}
