// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MARKDOWN COMPOSE (`docs/DESIGN-landing-economy.md` §2.1, slice 1): a moved main whose
//! only shared paths are Markdown no build, include or test reads is merged and rechecked with
//! the source readers, never judged in full; every other shared path is judged in full. Each
//! test sits beside the control that shows the rule bites.

use super::*;
use aterm_types::land_trailer::Rechecked;

/// The Markdown the worlds below share, eight lines each: a name no live tree holds, so this
/// file names no live no-reach path in code and never stops its own landing's compose
/// ([`the_live_tree_names_the_known_issues_in_no_code`]).
pub(super) const DOC: &str = "docs/FIXTURE.md";

/// A file a world's main also holds: its path, its text ([`SHARED`] where `None`) and whether it
/// is executable.
pub(super) type File<'a> = (&'a str, Option<&'a str>, bool);

/// A case of a shared path the rule judges in full: its name, the files main gains first, the
/// paths both sides then change, and whether one side renames them instead.
type Case<'a> = (&'a str, Vec<File<'a>>, Vec<&'a str>, bool);

/// [`world`] whose main also holds [`DOC`] and each of `files` (path, text, and whether it is
/// executable), all of [`SHARED`]'s eight lines unless given, Rust files that name `DOC` only
/// where a no-reach Markdown file may be named (a comment, and census's own guards), and the
/// no-reach rule this land runs at its path. The work branch starts from it.
pub(super) fn markdown_world(files: &[File<'_>]) -> World {
    let world = world();
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(
        &world.rival,
        &["checkout", "-q", "-B", "main", "origin/main"],
    );
    write(&world.rival, DOC, SHARED);
    write(
        &world.rival,
        "crates/a/src/lib.rs",
        &format!("// The known issues live in {DOC}.\npub fn a() {{}}\n"),
    );
    write(
        &world.rival,
        "crates/aterm-census/src/guard.rs",
        &format!("pub const POLICED: &str = \"{DOC}\";\n"),
    );
    write(&world.rival, RULE_PATH, aterm_census::no_reach::SOURCE);
    for (file, text, _) in files {
        write(&world.rival, file, text.unwrap_or(SHARED));
    }
    git(&world.rival, &["add", "-A"]);
    for (file, _, executable) in files {
        if *executable {
            // The index's mode, and on a host whose checkout keeps modes the file's too, so a
            // later `add -A` there keeps it.
            git(&world.rival, &["add", "--chmod=+x", "--", file]);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let at = world.rival.join(file);
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
        }
    }
    git(&world.rival, &["commit", "-q", "-m", "seed the Markdown"]);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    world
}

/// `file` in `dir` with its 1-based `line` replaced by `text`, committed: the commit.
pub(super) fn edit_line(dir: &Path, file: &str, line: usize, text: &str) -> String {
    let now = std::fs::read_to_string(dir.join(file)).expect("the file");
    let mut lines: Vec<&str> = now.lines().collect();
    lines[line - 1] = text;
    commit(dir, file, &format!("{}\n", lines.join("\n")))
}

/// Move origin's main from the rival clone by `edit` of the rival's fresh main: the new tip.
pub(super) fn rival_does(world: &World, edit: impl FnOnce(&Path)) -> String {
    git(&world.rival, &["fetch", "-q", "origin"]);
    git(
        &world.rival,
        &["checkout", "-q", "-B", "main", "origin/main"],
    );
    edit(&world.rival);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    git(&world.rival, &["rev-parse", "HEAD"])
}

/// A recheck that ran the source readers and passed.
pub(super) fn rechecked() -> Judged {
    ran(
        exit::PASS,
        RECHECKS.into_iter().map(|check| row(check, "ok")).collect(),
        2,
    )
}

/// The branch edits `DOC`'s first line and its own code; main moves `DOC`'s last line while the
/// judgment runs. The pass is merged with that main and rechecked once, over the cone since it,
/// on the merge of the judged commit and that main; the recheck's tree is stated as
/// `Aterm-Rechecked` and no second judgment runs. A main that moves again while the recheck runs,
/// disjointly, composes onto the rechecked merge at once. NEGATIVE CONTROL: the same Markdown
/// move beside a move of a code file the branch also changed is judged in full and nothing is
/// rechecked.
#[test]
fn a_markdown_only_overlap_composes_after_a_recheck_and_a_later_disjoint_move_composes_too() {
    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    let judged_at = git(&world.work, &["rev-parse", "HEAD"]);
    let (moved, later) = (RefCell::new(String::new()), RefCell::new(String::new()));
    let (code, said, bases, rechecks) = land_rechecking(
        &world,
        &git_cmd,
        crates(&["a"]),
        Lane::Pass,
        |_, n, _| {
            if n == 0 {
                *moved.borrow_mut() =
                    rival_does(&world, |rival| drop(edit_line(rival, DOC, 8, "eight")));
            }
            Ran::Ended(green())
        },
        |_| {
            *later.borrow_mut() = rival_moves(&world, "b/y.txt", "b2\n");
            rechecked()
        },
    );
    let (moved, later) = (moved.into_inner(), later.into_inner());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "rechecked, not judged again: {said:#?}");
    assert_eq!(rechecks.len(), 1, "{said:#?}");
    let (base, merge) = &rechecks[0];
    assert_eq!(
        base, &moved,
        "the recheck's cone starts at the main it merged"
    );
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{merge}^1")]),
        judged_at
    );
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{merge}^2")]),
        moved
    );
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^1")]),
        *merge
    );
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^2")]),
        later
    );
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(receipt.checked.len(), 1);
    assert_eq!(
        receipt.rechecked,
        [Rechecked {
            tree: git(&world.work, &["rev-parse", &format!("{merge}^{{tree}}")]),
            rows: RECHECKS
                .into_iter()
                .map(|check| Row {
                    check: check.into(),
                    word: "ok".into(),
                })
                .collect(),
            base: moved.clone(),
            onto: moved.clone(),
            secs: 2,
            after: 1,
        }]
    );
    assert_eq!(receipt.composed_onto.as_deref(), Some(later.as_str()));
    assert_eq!(receipt.rejudges(), 0);
    let doc = git(&world.work, &["show", &format!("{landing}:{DOC}")]);
    assert!(
        doc.starts_with("one\n") && doc.ends_with("\neight"),
        "both edits land: {doc}"
    );
    assert!(
        said.iter().any(|line| line.contains(&format!(
            "LANDED {landing} on origin/main: checked {} (0 re-judges), rechecked onto {moved}, \
             composed onto {later}",
            receipt.checked[0].tree
        ))),
        "{said:#?}"
    );

    let control = markdown_world(&[]);
    commit(&control.work, "a/x.txt", "a2\n");
    edit_line(&control.work, DOC, 1, "one");
    edit_line(&control.work, "shared.txt", 1, "one");
    let (code, said, bases, rechecks) = land_rechecking(
        &control,
        &git_cmd,
        crates(&["a"]),
        Lane::Pass,
        |_, n, _| {
            if n == 0 {
                rival_does(&control, |rival| {
                    edit_line(rival, DOC, 8, "eight");
                    drop(edit_line(rival, "shared.txt", 8, "eight"));
                });
            }
            Ran::Ended(green())
        },
        |base| panic!("Markdown beside a shared code file is never rechecked ({base})"),
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert!(rechecks.is_empty(), "{said:#?}");
    assert_eq!(bases.len(), 2, "judged in full: {said:#?}");
}

/// Every shared path outside the class, or in it but no regular file on all three sides, or
/// named by code, is judged in full: an image, a fork's or a package's Markdown, TOML, an
/// executable, Markdown a Rust string names, a lock beside Markdown, and a rename on either
/// side. NEGATIVE CONTROL: [`DOC`] alone, named only by a comment and by census's guards,
/// composes after one recheck.
#[test]
fn a_shared_path_outside_the_markdown_rule_is_judged_in_full() {
    let named = "pub const NAMED: &str = \"docs/NAMED.md\";\n";
    let cases: [Case<'_>; 10] = [
        (
            "an image",
            vec![("docs/shot.png", None, false)],
            vec!["docs/shot.png"],
            false,
        ),
        (
            "a fork's Markdown",
            vec![("vendor/x/README.md", None, false)],
            vec!["vendor/x/README.md"],
            false,
        ),
        (
            "TOML",
            vec![("deny.toml", None, false)],
            vec!["deny.toml"],
            false,
        ),
        (
            "docs TOML",
            vec![("docs/conf.toml", None, false)],
            vec!["docs/conf.toml"],
            false,
        ),
        (
            "a package's docs",
            vec![
                ("docs/sub/Cargo.toml", Some("[package]\n"), false),
                ("docs/sub/x.md", None, false),
            ],
            vec!["docs/sub/x.md"],
            false,
        ),
        (
            "an executable",
            vec![("docs/run.md", None, true)],
            vec!["docs/run.md"],
            false,
        ),
        (
            "named in code",
            vec![
                ("docs/NAMED.md", None, false),
                ("crates/b/src/lib.rs", Some(named), false),
            ],
            vec!["docs/NAMED.md"],
            false,
        ),
        (
            "a lock beside Markdown",
            vec![("Cargo.lock", None, false)],
            vec!["Cargo.lock", DOC],
            false,
        ),
        ("the branch renames", vec![], vec![DOC], true),
        ("main renames", vec![], vec![DOC], true),
    ];
    for (case, files, shared, rename) in cases {
        let world = markdown_world(&files);
        commit(&world.work, "a/x.txt", "a2\n");
        let branch_renames = case == "the branch renames";
        for file in &shared {
            if rename && branch_renames {
                git(&world.work, &["mv", file, "docs/MOVED.md"]);
                git(&world.work, &["commit", "-q", "-m", "move it"]);
            } else {
                edit_line(&world.work, file, 1, "one");
            }
        }
        let (code, said, bases, rechecks) = land_rechecking(
            &world,
            &git_cmd,
            crates(&["a"]),
            Lane::Pass,
            |_, n, _| {
                if n == 0 {
                    rival_does(&world, |rival| {
                        for file in &shared {
                            if rename && !branch_renames {
                                git(rival, &["mv", file, "docs/MOVED.md"]);
                                git(rival, &["commit", "-q", "-m", "move it"]);
                            } else {
                                edit_line(rival, file, 8, "eight");
                            }
                        }
                    });
                }
                Ran::Ended(green())
            },
            |base| panic!("{case}: never rechecked ({base})"),
        );
        assert_eq!(code, exit::PASS, "{case}: {said:#?}");
        assert!(rechecks.is_empty(), "{case}: {said:#?}");
        assert_eq!(bases.len(), 2, "{case}: judged in full: {said:#?}");
        // Where every shared path is Markdown, the line names the part of the rule that failed.
        let why = match case {
            "a fork's Markdown" | "a package's docs" => Some("is not Markdown no build"),
            "an executable" | "the branch renames" | "main renames" => {
                Some("is not one regular file in")
            }
            "named in code" => Some("is named in crates/b/src/lib.rs's code"),
            _ => None,
        };
        if let Some(why) = why {
            assert!(
                said.iter()
                    .any(|line| line.contains("does not compose") && line.contains(why)),
                "{case}: {said:#?}"
            );
        }
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        assert!(receipt.rechecked.is_empty(), "{case}");
    }

    let control = markdown_world(&[]);
    commit(&control.work, "a/x.txt", "a2\n");
    edit_line(&control.work, DOC, 1, "one");
    let (code, said, bases, rechecks) = land_rechecking(
        &control,
        &git_cmd,
        crates(&["a"]),
        Lane::Pass,
        |_, n, _| {
            if n == 0 {
                rival_does(&control, |rival| drop(edit_line(rival, DOC, 8, "eight")));
            }
            Ran::Ended(green())
        },
        |_| rechecked(),
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!((bases.len(), rechecks.len()), (1, 1), "{said:#?}");
}

/// A recheck reads the merge with main's own rules: a census rule main added beside its
/// Markdown edit fails the branch's Markdown there, and nothing is pushed. Any red counts (no
/// lane is read), a check that did not run or was skipped, or a run of other checks, could not
/// run, and a checkout that changed during the recheck pushes nothing. The control is the green
/// recheck of the first test.
#[test]
fn a_recheck_red_pushes_nothing_and_one_that_did_not_run_could_not_run() {
    let rule = "crates/aterm-census/src/guards/new_rule.rs";
    let reds: [(&str, Judged, i32); 5] = [
        (
            "main's new rule fails the branch's Markdown",
            ran(
                exit::FAILED,
                vec![
                    row("fmt", "ok"),
                    red("census", &[&format!("{DOC}:1")]),
                    row("export-scan", "ok"),
                ],
                3,
            ),
            exit::FAILED,
        ),
        (
            "census did not run",
            ran(
                exit::COULD_NOT_RUN,
                vec![
                    row("fmt", "ok"),
                    row("census", "NOT RUN"),
                    row("export-scan", "ok"),
                ],
                3,
            ),
            exit::COULD_NOT_RUN,
        ),
        (
            "export-scan was skipped",
            ran(
                exit::PASS,
                vec![
                    row("fmt", "ok"),
                    row("census", "ok"),
                    row("export-scan", "skip"),
                ],
                3,
            ),
            exit::COULD_NOT_RUN,
        ),
        (
            "another set of checks ran",
            ran(exit::PASS, vec![row("fmt", "ok"), row("census", "ok")], 3),
            exit::COULD_NOT_RUN,
        ),
        (
            "the checkout changed meanwhile",
            rechecked(),
            exit::COULD_NOT_RUN,
        ),
    ];
    for (case, judged, want) in reds {
        let world = markdown_world(&[]);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, DOC, 1, "one");
        let judged = RefCell::new(Some(judged));
        let moved = RefCell::new(String::new());
        let (code, said, bases, rechecks) = land_rechecking(
            &world,
            &git_cmd,
            crates(&["a"]),
            Lane::Pass,
            |_, n, _| {
                if n == 0 {
                    *moved.borrow_mut() = rival_does(&world, |rival| {
                        write(rival, rule, "// a rule the branch's Markdown breaks\n");
                        drop(edit_line(rival, DOC, 8, "eight"));
                    });
                }
                Ran::Ended(green())
            },
            |_| {
                assert!(
                    world.work.join(rule).exists(),
                    "{case}: the recheck runs on the merge, main's rules in it"
                );
                if case == "the checkout changed meanwhile" {
                    write(&world.work, "stray.txt", "x\n");
                }
                judged.borrow_mut().take().expect("one recheck")
            },
        );
        assert_eq!(code, want, "{case}: {said:#?}");
        assert_eq!((bases.len(), rechecks.len()), (1, 1), "{case}: {said:#?}");
        assert_eq!(
            origin_main(&world),
            moved.borrow().as_str(),
            "{case}: nothing pushed"
        );
        assert!(
            said.iter().any(|line| line.contains("nothing was pushed")),
            "{case}: {said:#?}"
        );
    }
}

/// A Markdown-only tip read while the judgment runs interrupts nothing (slice 0's poll treats it
/// as composing); the pass is rechecked at the push. The control is slice 0's own test: a tip
/// that touches the branch's code interrupts the run.
#[test]
fn a_markdown_only_tip_does_not_interrupt_the_running_judgment() {
    let world = markdown_world(&[]);
    edit_line(&world.work, DOC, 1, "one");
    commit(&world.work, "a/x.txt", "a2\n");
    let mut polls = Vec::new();
    let (code, said, bases, rechecks) = land_rechecking(
        &world,
        &git_cmd,
        crates(&["a"]),
        Lane::Pass,
        |_, n, poll| {
            if n == 0 {
                rival_does(&world, |rival| drop(edit_line(rival, DOC, 8, "eight")));
                polls.push(poll(&mut || false));
                polls.push(poll(&mut || false));
            }
            Ran::Ended(green())
        },
        |_| rechecked(),
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(polls, [false, false], "{said:#?}");
    assert_eq!((bases.len(), rechecks.len()), (1, 1), "{said:#?}");
    let receipt = stated(&world, &origin_main(&world)).expect("stated");
    assert!(receipt.interrupted.is_empty());
}

/// A worktree merge whose tree is not the one the Markdown rule tested (here a merge driver that
/// keeps the branch's side, which only the tool's own merge sees) is judged in full, as one re-judge, and nothing is rechecked.
/// The control is the first test: the same move, merged as tested, is rechecked.
#[test]
fn a_merge_unlike_the_tested_tree_is_judged_in_full() {
    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    let attributes = world
        .work
        .parent()
        .expect("scratch")
        .join("ours.attributes");
    std::fs::write(&attributes, format!("{DOC} merge=ours\n")).expect("attributes");
    let armed = std::cell::Cell::new(false);
    let seam = |dir: &Path| {
        let mut command = git_cmd(dir);
        if armed.get() {
            command.args([
                "-c",
                &format!("core.attributesFile={}", path(&attributes)),
                "-c",
                "merge.ours.driver=true",
            ]);
        }
        command
    };
    let said = RefCell::new(Vec::new());
    let judged = std::cell::Cell::new(0);
    let code = land(
        &world.work,
        &own(),
        &mut Seams {
            git: &seam,
            verifier: &mut |_: &Path| Ok(own_image()),
            judge: &mut |_: &Path, _: &Path, _: &str, _: Poll<'_>| {
                if judged.replace(judged.get() + 1) == 0 {
                    rival_does(&world, |rival| drop(edit_line(rival, DOC, 8, "eight")));
                }
                Ran::Ended(green())
            },
            branch: &mut |_: &Path, _: &str| crates(&["a"]),
            lane: &mut |_: &Path, _: &str| Lane::Pass,
            recheck: &mut |_: &Path, _: &Path, _: &str| {
                panic!("a merge unlike the tested tree is judged")
            },
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| {
                // The rule has tested the plumbing's tree: only the worktree merge after it
                // sees the attribute.
                if line.contains("composes once") {
                    armed.set(true);
                }
                said.borrow_mut().push(line.to_owned());
            },
            remote: &crate::remote::run,
            pause: &no_retry,
        },
    );
    let said = said.into_inner();
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(judged.get(), 2, "{said:#?}");
    assert!(
        said.iter()
            .any(|line| line.contains("the Markdown rule tested: it is judged in full")),
        "{said:#?}"
    );
    let receipt = stated(&world, &origin_main(&world)).expect("stated");
    assert!(receipt.rechecked.is_empty());
    assert_eq!(receipt.rejudges(), 1);
}

/// Rechecks count toward [`COMPOSES`] with compositions: a main that keeps moving its Markdown
/// stops the landing after that many rechecks, merged and rechecked, with nothing pushed.
#[test]
fn rechecks_count_toward_the_composition_bound() {
    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    let moves = std::cell::Cell::new(0);
    let move_doc = || {
        moves.set(moves.get() + 1);
        let text = format!("eight {}", moves.get());
        rival_does(&world, |rival| drop(edit_line(rival, DOC, 8, &text)))
    };
    let (code, said, bases, rechecks) = land_rechecking(
        &world,
        &git_cmd,
        crates(&["a"]),
        Lane::Pass,
        |_, _, _| {
            move_doc();
            Ran::Ended(green())
        },
        |_| {
            move_doc();
            rechecked()
        },
    );
    assert_eq!(code, STOPPED, "{said:#?}");
    assert_eq!((bases.len(), rechecks.len()), (1, COMPOSES), "{said:#?}");
    assert_eq!(
        origin_main(&world),
        git(&world.rival, &["rev-parse", "HEAD"])
    );
    assert!(
        said.iter()
            .any(|line| line.contains(&format!("STOPPED after {COMPOSES} push/composition races"))),
        "{said:#?}"
    );
}

/// Land `world`'s branch (judged green) while main makes `rival`'s commit once: it must be
/// judged in full, never rechecked, and say `why` it does not compose.
fn judged_in_full(world: &World, git: &dyn Fn(&Path) -> Command, rival: &dyn Fn(&Path), why: &str) {
    let (code, said, bases, rechecks) = land_rechecking(
        world,
        git,
        crates(&["a"]),
        Lane::Pass,
        |_, n, _| {
            if n == 0 {
                rival_does(world, rival);
            }
            Ran::Ended(green())
        },
        |base| panic!("{why}: never rechecked ({base})"),
    );
    assert_eq!(code, exit::PASS, "{why}: {said:#?}");
    assert!(rechecks.is_empty(), "{why}: {said:#?}");
    assert_eq!(bases.len(), 2, "{why}: judged in full: {said:#?}");
    assert!(
        said.iter()
            .any(|line| line.contains("does not compose") && line.contains(why)),
        "{why}: {said:#?}"
    );
    let receipt = stated(world, &origin_main(world)).expect("stated");
    assert!(receipt.rechecked.is_empty(), "{why}");
}

/// `git mv` of `from` to `to` in `dir`, beside `extra` files, committed.
fn move_into(dir: &Path, from: &str, to: &str, extra: &[(&str, &str)]) {
    std::fs::create_dir_all(dir.join(to).parent().expect("a parent")).expect("dirs");
    git(dir, &["mv", from, to]);
    for (file, text) in extra {
        write(dir, file, text);
    }
    git(dir, &["add", "-A"]);
    git(
        dir,
        &["commit", "-q", "-m", &format!("move {from} to {to}")],
    );
}

/// merge-ort follows renames, so the shared paths are also the paths the merge holds unlike
/// both sides: a Markdown file one side moves into a crate that `include_str!`s it, while the
/// other edits it at its old path, is judged in full for the crate's copy, whichever side moves
/// it; and with `merge.directoryRenames`, a file the branch adds under a directory main moves
/// into a crate is judged in full though no path is shared by name but [`DOC`], which alone
/// composes. The control is the first test's compose.
#[test]
fn a_rename_into_a_crate_is_judged_in_full_whichever_side_makes_it() {
    let moved = "crates/c/NOTES.md";
    let include = (
        "crates/c/src/lib.rs",
        "pub const NOTES: &str = include_str!(\"../NOTES.md\");\n",
    );
    let why = format!("{moved} is not Markdown no build, include or test reads");

    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    judged_in_full(
        &world,
        &git_cmd,
        &|rival| move_into(rival, DOC, moved, &[include]),
        &why,
    );
    let landed = git(
        &world.work,
        &["show", &format!("{}:{moved}", origin_main(&world))],
    );
    assert!(
        landed.starts_with("one\n"),
        "the merge followed the move: {landed}"
    );

    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    move_into(&world.work, DOC, moved, &[include]);
    judged_in_full(
        &world,
        &git_cmd,
        &|rival| drop(edit_line(rival, DOC, 8, "eight")),
        &why,
    );

    let world = markdown_world(&[("docs/old/a.md", None, false)]);
    git(&world.work, &["config", "merge.directoryRenames", "true"]);
    commit(&world.work, "a/x.txt", "a2\n");
    commit(&world.work, "docs/old/new.md", "new\n");
    edit_line(&world.work, DOC, 1, "one");
    judged_in_full(
        &world,
        &git_cmd,
        &|rival| {
            move_into(rival, "docs/old", "crates/c/docs", &[]);
            drop(edit_line(rival, DOC, 8, "eight"));
        },
        "crates/c/docs/new.md is not Markdown no build, include or test reads",
    );
}

/// The class this land chose the shared paths by must be the one census's guard holds the merge
/// to: a main that changes the no-reach rule beside a Markdown edit is judged in full, whatever
/// the new rule says. The control is the first test, whose merge holds this rule.
#[test]
fn a_main_that_changes_the_no_reach_rule_is_judged_in_full() {
    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    judged_in_full(
        &world,
        &git_cmd,
        &|rival| {
            write(
                rival,
                RULE_PATH,
                &format!("{}// a narrower class\n", aterm_census::no_reach::SOURCE),
            );
            drop(edit_line(rival, DOC, 8, "eight"));
        },
        &format!("the merge's {RULE_PATH} is not the rule this land chose the shared paths by"),
    );
}

/// The land-time search finds the readers census's guard and a whole-path search miss: a path
/// joined from pieces under a helper's root, a crate's own spec table read from its directory, a
/// doctest, a `#[doc]`-included README's Rust block, and a reader found through a Git that a
/// person's `GIT_LITERAL_PATHSPECS` or `GIT_NOGLOB_PATHSPECS` would leave matching no `*.rs`
/// pathspec. Each is judged in full. The control is the first test, whose `DOC` is named only by
/// a comment and census's guards.
#[test]
fn a_reader_only_the_file_name_finds_is_judged_in_full() {
    let workspace = "[workspace]\nmembers = [\"crates/*\"]\n";
    let joined =
        "pub fn r() -> std::path::PathBuf {\n    root().join(\"docs\").join(\"FIXTURE.md\")\n}\n";
    let doctest = "/// ```\n/// std::fs::read(concat!(env!(\"CARGO_MANIFEST_DIR\"), \
                   \"/../../docs/FIXTURE.md\")).unwrap();\n/// ```\npub fn d() {}\n";
    let readme = "# b\n\n```rust\nstd::fs::read(\"../../docs/FIXTURE.md\").unwrap();\n```\n";
    let own = "#[test]\nfn rows() {\n    std::fs::read_to_string(\"SPECS.md\").unwrap();\n}\n";
    let cases: [(&str, Vec<File<'_>>, &str); 4] = [
        (
            "crates/b/src/lib.rs",
            vec![("crates/b/src/lib.rs", Some(joined), false)],
            DOC,
        ),
        (
            "crates/b/src/lib.rs",
            vec![("crates/b/src/lib.rs", Some(doctest), false)],
            DOC,
        ),
        (
            "crates/b/README.md",
            vec![
                (
                    "crates/b/src/lib.rs",
                    Some("#![doc = include_str!(\"../README.md\")]\n"),
                    false,
                ),
                ("crates/b/README.md", Some(readme), false),
            ],
            DOC,
        ),
        (
            "crates/b/tests/rows.rs",
            vec![
                ("Cargo.toml", Some(workspace), false),
                (
                    "crates/b/Cargo.toml",
                    Some("[package]\nname = \"b\"\n"),
                    false,
                ),
                ("crates/b/SPECS.md", None, false),
                ("crates/b/tests/rows.rs", Some(own), false),
            ],
            "crates/b/SPECS.md",
        ),
    ];
    for (reader, files, shared) in cases {
        let world = markdown_world(&files);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, shared, 1, "one");
        judged_in_full(
            &world,
            &git_cmd,
            &|rival| drop(edit_line(rival, shared, 8, "eight")),
            &format!("{shared} is named in {reader}'s code"),
        );
    }
    for mode in ["GIT_LITERAL_PATHSPECS", "GIT_NOGLOB_PATHSPECS"] {
        let world = markdown_world(&[("crates/b/src/lib.rs", Some(joined), false)]);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, DOC, 1, "one");
        let pathspecs = |dir: &Path| {
            let mut command = git_cmd(dir);
            command.env(mode, "1");
            command
        };
        judged_in_full(
            &world,
            &pathspecs,
            &|rival| drop(edit_line(rival, DOC, 8, "eight")),
            &format!("{DOC} is named in crates/b/src/lib.rs's code"),
        );
    }
}

/// The slice's main target composes on this checkout: no file outside census may read
/// `docs/KNOWN_ISSUES.md` by the land-time search ([`aterm_census::no_reach::may_read`]), so a
/// test fixture or helper that spells it in code (which would judge every overlap of it in full)
/// fails here. The path is spelled in two pieces so this file is not itself such a reader.
#[test]
fn the_live_tree_names_the_known_issues_in_no_code() {
    let path = concat!("docs/KNOWN", "_ISSUES.md");
    let root = aterm_tempfile::manifest_dir().join("../..");
    let mut readers = Vec::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(root.join(&dir)).expect("a checkout directory") {
            let entry = entry.expect("an entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let kind = entry.file_type().expect("a type");
            if kind.is_dir() {
                if !(name.starts_with('.')
                    || name.ends_with(".noindex")
                    || ["target", "vendor", "node_modules"].contains(&name.as_str()))
                {
                    dirs.push(rel);
                }
            } else if kind.is_file()
                // Only the files the search reads (no docs or spec table, whose text this test
                // must not depend on).
                && aterm_census::no_reach::searched(&rel)
                && let Ok(text) = std::fs::read_to_string(entry.path())
                && aterm_census::no_reach::may_read(&rel, &text, path)
            {
                readers.push(rel);
            }
        }
    }
    assert_eq!(
        readers,
        Vec::<String>::new(),
        "each of these spells {path} in code, so every landing that shares it is judged in full"
    );
}
