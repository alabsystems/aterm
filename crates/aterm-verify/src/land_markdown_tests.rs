// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROUND (`docs/DESIGN-landing-economy.md` §2.1): a moved main that shares paths with a
//! judged branch is merged and the merge judged over the cone of those paths, the judgment
//! standing for the tree it ran on. Shared Markdown no build, include or test reads is in no
//! cone, so the round's source readers alone answer for it; such Markdown that a workspace
//! member's file may read stands for that file, whose crate the round judges; and such
//! Markdown that only a file no member owns may read, or that is no regular file on every
//! side, is judged in full. Each test sits beside the control that shows the rule bites.

use super::*;
use aterm_land_trailer::Rechecked;

/// The Markdown the worlds below share, eight lines each: a name no live tree holds, so this
/// file names no live no-reach path in code and never stops its own landing's compose
/// ([`the_live_tree_names_the_composing_markdown_in_no_code`]).
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

/// A round whose cone held no crate: the source readers ran and passed.
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
/// move beside a move of a file the branch also changed, whose reach is every crate, is judged
/// in full and nothing is rechecked. The run records say so: a round of cause `shared` and a
/// `markdown` landing, and the control's re-judge of cause `doomed`, landed `exact`. NEGATIVE
/// CONTROLS, each run once as a mutant that made this test fail: a round of shared Markdown
/// alone named `rechecked`; a shared tip leaving the round no cause (it read `handed`).
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
        |_, n, _| {
            if n == 0 {
                rival_does(&control, |rival| {
                    edit_line(rival, DOC, 8, "eight");
                    drop(edit_line(rival, "shared.txt", 8, "eight"));
                });
            }
            Ran::Ended(green())
        },
        |base| panic!("shared paths that reach every crate are never rechecked ({base})"),
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert!(rechecks.is_empty(), "{said:#?}");
    assert_eq!(bases.len(), 2, "judged in full: {said:#?}");
    // The run records: the round the shared Markdown caused (cause `shared`), the compose after
    // it, and a `markdown` landing; the control's abort and re-judge, landed `exact`.
    assert_eq!(
        super::quietly(),
        [
            super::landed(
                &[
                    ("judgment", "pass", "first"),
                    ("round", "pass", "shared"),
                    ("compose", "composed", "disjoint"),
                    ("end", "landed", ""),
                ],
                "markdown",
            ),
            super::landed(
                &[
                    ("judgment", "pass", "first"),
                    ("abort", "doomed", "doomed"),
                    ("judgment", "pass", "doomed"),
                    ("end", "landed", ""),
                ],
                "exact",
            ),
        ]
    );
}

/// A shared path a crate may own (an image, a fork's or a package's Markdown, TOML, a lock
/// beside Markdown) does not throw the judgment away: the judgment stands, and one round judges
/// the merge, the reach asked only where a shared path is one a crate may own.
/// The round's rows, a package's among them, are stated as `Aterm-Rechecked`. NEGATIVE CONTROLS:
/// shared no-reach Markdown that is executable, that a Rust string of a file no workspace
/// member owns names, or that either side renames is in no cone and not the source readers'
/// alone to answer for, so it is judged in full and nothing is rechecked; and [`DOC`] alone,
/// named only by a comment and by census's guards, is answered by a round that asks no reach.
/// The run records name the landing of a round of shared code `rechecked`, and that of
/// [`DOC`]'s round `markdown`.
#[test]
fn a_shared_code_path_is_judged_over_its_cone_on_the_merge() {
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
        // Why the case is judged in full, where it is.
        let whole = match case {
            "an executable" | "the branch renames" | "main renames" => {
                Some("is not one regular file in")
            }
            "named in code" => Some("is named in crates/b/src/lib.rs's code"),
            _ => None,
        };
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
        let asked = RefCell::new(Vec::new());
        let reach = |paths: Option<&[String]>| match paths {
            None => reaches(&["a", "b"]),
            Some(paths) => {
                asked.borrow_mut().push(paths.to_vec());
                reaches(&["b"])
            }
        };
        let deltas = RefCell::new(Vec::new());
        let (code, said, bases, rechecks) = land_reaching(
            &world,
            &reach,
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
            |_, delta| {
                assert!(whole.is_none(), "{case}: never rechecked");
                deltas.borrow_mut().push(delta.shared.clone());
                ran(
                    exit::PASS,
                    vec![
                        row("fmt", "ok"),
                        row("census", "ok"),
                        row("export-scan", "ok"),
                        row("lint", "ok"),
                        row("test", "ok"),
                    ],
                    4,
                )
            },
        );
        assert_eq!(code, exit::PASS, "{case}: {said:#?}");
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        if let Some(why) = whole {
            assert!(rechecks.is_empty(), "{case}: {said:#?}");
            assert_eq!(bases.len(), 2, "{case}: judged in full: {said:#?}");
            assert!(
                said.iter()
                    .any(|line| line.contains("does not compose") && line.contains(why)),
                "{case}: {said:#?}"
            );
            assert!(receipt.rechecked.is_empty(), "{case}");
            assert!(asked.borrow().is_empty(), "{case}: no reach is asked");
            continue;
        }
        assert_eq!(
            (bases.len(), rechecks.len()),
            (1, 1),
            "{case}: the judgment stands and one round follows: {said:#?}"
        );
        let all: Vec<String> = shared.iter().map(|path| (*path).to_owned()).collect();
        assert_eq!(*deltas.borrow(), std::slice::from_ref(&all), "{case}");
        let owned: Vec<String> = all.into_iter().filter(|path| path != DOC).collect();
        assert!(
            !asked.borrow().is_empty() && asked.borrow().iter().all(|paths| *paths == owned),
            "{case}: the reach is asked of the paths a crate may own: {:?}",
            asked.borrow()
        );
        assert_eq!(receipt.checked.len(), 1, "{case}");
        assert_eq!(receipt.rechecked.len(), 1, "{case}");
        assert!(
            receipt.rechecked[0]
                .rows
                .iter()
                .any(|row| row.check == "test" && row.word == "ok"),
            "{case}: the round's package rows are stated"
        );
    }

    let control = markdown_world(&[]);
    commit(&control.work, "a/x.txt", "a2\n");
    edit_line(&control.work, DOC, 1, "one");
    let (code, said, bases, rechecks) = land_reaching(
        &control,
        &|paths| match paths {
            None => reaches(&["a"]),
            Some(paths) => panic!("Markdown no code reads has no reach to ask ({paths:?})"),
        },
        |_, n, _| {
            if n == 0 {
                rival_does(&control, |rival| drop(edit_line(rival, DOC, 8, "eight")));
            }
            Ran::Ended(green())
        },
        |_, delta| {
            assert_eq!(delta.shared, [DOC]);
            rechecked()
        },
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!((bases.len(), rechecks.len()), (1, 1), "{said:#?}");
    // The run records: a round of a shared code path lands `rechecked`, and the round of DOC
    // alone `markdown`.
    let told = super::quietly();
    let round = |kind| {
        super::landed(
            &[
                ("judgment", "pass", "first"),
                ("round", "pass", "shared"),
                ("end", "landed", ""),
            ],
            kind,
        )
    };
    assert_eq!(told.first(), Some(&round("rechecked")), "{told:#?}");
    assert_eq!(told.last(), Some(&round("markdown")), "{told:#?}");
}

/// A round reads the merge with main's own rules: a census rule main added beside its Markdown
/// edit fails the branch's Markdown there, and nothing is pushed. A red counts as in a judgment
/// (a package's too), a source reader that did not run, was
/// skipped or is missing from the run could not run, and a checkout that changed during the
/// round pushes nothing. NEGATIVE CONTROL: the source readers' passes beside other rows that
/// passed or were skipped land.
#[test]
fn a_recheck_red_pushes_nothing_and_one_that_did_not_run_could_not_run() {
    let rule = "crates/aterm-census/src/guards/new_rule.rs";
    let readers = || {
        vec![
            row("fmt", "ok"),
            row("census", "ok"),
            row("export-scan", "ok"),
        ]
    };
    let with = |more: Vec<CheckRow>| {
        let mut rows = readers();
        rows.extend(more);
        rows
    };
    let reds: [(&str, Judged, i32); 7] = [
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
            "a source reader is missing from the run",
            ran(
                exit::PASS,
                vec![row("fmt", "ok"), row("census", "ok"), row("test", "ok")],
                3,
            ),
            exit::COULD_NOT_RUN,
        ),
        (
            "a package's test fails",
            ran(
                exit::FAILED,
                with(vec![red("test", &["-p a --lib -- mine"])]),
                3,
            ),
            exit::FAILED,
        ),
        (
            "the checkout changed meanwhile",
            rechecked(),
            exit::COULD_NOT_RUN,
        ),
        (
            "other rows passed or were skipped",
            ran(
                exit::PASS,
                with(vec![
                    row("lint", "skip"),
                    row("test", "ok"),
                    row("doc", "ok"),
                ]),
                3,
            ),
            exit::PASS,
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
                    "{case}: the round runs on the merge, main's rules in it"
                );
                if case == "the checkout changed meanwhile" {
                    write(&world.work, "stray.txt", "x\n");
                }
                judged.borrow_mut().take().expect("one round")
            },
        );
        assert_eq!(code, want, "{case}: {said:#?}");
        assert_eq!((bases.len(), rechecks.len()), (1, 1), "{case}: {said:#?}");
        if want == exit::PASS {
            let receipt = stated(&world, &origin_main(&world)).expect("stated");
            assert_eq!(receipt.rechecked.len(), 1, "{case}");
            assert_eq!(receipt.rechecked[0].rows.len(), 6, "{case}");
            continue;
        }
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

/// A worktree merge whose tree is not the one the shared paths were read from (here a merge
/// driver that keeps the branch's side, which only the tool's own merge sees) is judged in full,
/// as one re-judge, and nothing is rechecked.
/// The control is the first test: the same move, merged as tested, is rechecked. The run
/// records name the re-judge's cause `merge-differs`. NEGATIVE CONTROL, run once as a mutant
/// that made this test fail: the differing merge leaving the re-judge no cause (it read
/// `first`).
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
            records: super::quiet(),
            reach: &|_: &Path, _: &str, _: Option<&[String]>| {
                panic!("shared Markdown no code reads has no reach to ask")
            },
            recheck: &mut |_: &Path, _: &Path, _: &str, _: &Delta| {
                panic!("a merge unlike the tested tree is judged")
            },
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| {
                // The rule has tested the plumbing's tree: only the worktree merge after it
                // sees the attribute.
                if line.contains("the judgment stands for the tree it ran on") {
                    armed.set(true);
                }
                said.borrow_mut().push(line.to_owned());
            },
            remote: &live_remote,
            pause: &no_retry,
        },
    );
    let said = said.into_inner();
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(judged.get(), 2, "{said:#?}");
    assert!(
        said.iter()
            .any(|line| line
                .contains("the tree the shared paths were read from: it is judged in full")),
        "{said:#?}"
    );
    let receipt = stated(&world, &origin_main(&world)).expect("stated");
    assert!(receipt.rechecked.is_empty());
    assert_eq!(receipt.rejudges(), 1);
    // The run records: the re-judge the differing merge caused, of cause `merge-differs`.
    assert_eq!(
        super::quietly(),
        [super::landed(
            &[
                ("judgment", "pass", "first"),
                ("judgment", "pass", "merge-differs"),
                ("end", "landed", ""),
            ],
            "exact",
        )]
    );
}

/// A round that passes renews the composition bound ([`COMPOSES`]): it judged the merge of the
/// tip that raced the push, so a main that moves its Markdown during the judgment and during
/// each of more rounds than the bound holds is merged and judged by one round each time, and
/// the landing lands once main rests, never stopped at the bound. The control is the bound
/// itself, which a main that only composes still reaches
/// (`a_main_that_moves_before_every_push_is_composed_onto_and_lands_within_the_bound`).
#[test]
fn a_round_that_passes_renews_the_composition_bound() {
    const ROUNDS: usize = COMPOSES + 2;
    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    let moves = std::cell::Cell::new(0);
    let move_doc = || {
        if moves.get() == ROUNDS {
            return;
        }
        moves.set(moves.get() + 1);
        let text = format!("eight {}", moves.get());
        rival_does(&world, |rival| drop(edit_line(rival, DOC, 8, &text)));
    };
    let (code, said, bases, rechecks) = land_rechecking(
        &world,
        &git_cmd,
        crates(&["a"]),
        |_, _, _| {
            move_doc();
            Ran::Ended(green())
        },
        |_| {
            move_doc();
            rechecked()
        },
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!((bases.len(), rechecks.len()), (1, ROUNDS), "{said:#?}");
    assert!(
        !said.iter().any(|line| line.contains("STOPPED")),
        "{said:#?}"
    );
    let receipt = stated(&world, &origin_main(&world)).expect("stated");
    assert_eq!(
        (receipt.checked.len(), receipt.rechecked.len()),
        (1, ROUNDS)
    );
}

/// Land `world`'s branch (judged green) while main makes `rival`'s commit once: it must be
/// judged in full, never rechecked, and say `why` it does not compose.
fn judged_in_full(world: &World, git: &dyn Fn(&Path) -> Command, rival: &dyn Fn(&Path), why: &str) {
    let (code, said, bases, rechecks) = land_rechecking(
        world,
        git,
        crates(&["a"]),
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
/// other edits it at its old path, is judged in full, whichever side moves it, since its old
/// path is in no cone and no regular file on every side; and with `merge.directoryRenames`, a
/// file the branch adds under a directory main moves into a crate is judged in full for the
/// path the branch gave it, though no path is shared by name but [`DOC`], which alone is
/// answered by a round. The control is the first test's round.
#[test]
fn a_rename_into_a_crate_is_judged_in_full_whichever_side_makes_it() {
    let moved = "crates/c/NOTES.md";
    let include = (
        "crates/c/src/lib.rs",
        "pub const NOTES: &str = include_str!(\"../NOTES.md\");\n",
    );
    // The Markdown's old path is in no cone, and one side no longer holds it.
    let why = format!("{DOC} is not one regular file in");

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
        "docs/old/new.md is not one regular file in",
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

/// A workspace whose members are `crates/*`.
pub(super) const WORKSPACE: &str = "[workspace]\nmembers = [\"crates/*\"]\n";
/// The spec table of the member `b`, which [`ROWS`] reads from its crate's directory.
pub(super) const SPEC: &str = "crates/b/SPECS.md";
/// `b`'s integration test, which reads [`SPEC`].
pub(super) const ROWS: &str = "crates/b/tests/rows.rs";
/// [`ROWS`]' text.
pub(super) const READS_SPEC: &str =
    "#[test]\nfn rows() {\n    std::fs::read_to_string(\"SPECS.md\").unwrap();\n}\n";

/// [`markdown_world`] whose main is a workspace with the member `b`, its spec table [`SPEC`]
/// (executable where `executable`) and the test [`ROWS`] that reads it, and each of `more`.
fn spec_world(executable: bool, more: &[File<'_>]) -> World {
    let mut files: Vec<File<'_>> = vec![
        ("Cargo.toml", Some(WORKSPACE), false),
        (
            "crates/b/Cargo.toml",
            Some("[package]\nname = \"b\"\n"),
            false,
        ),
        (SPEC, None, executable),
        (ROWS, Some(READS_SPEC), false),
    ];
    files.extend_from_slice(more);
    markdown_world(&files)
}

/// SHARED MARKDOWN A MEMBER'S FILE MAY READ IS JUDGED BY A ROUND OVER ITS READERS, and cuts no
/// running judgment: the branch and main each change a row of a crate's own spec table, which
/// that crate's test reads from its directory (the landing this rule was written for threw a
/// 26-minute judgment away there). The tip polled while the judgment runs interrupts nothing;
/// the judgment stands, and one round judges the merge, its delta naming the Markdown and the
/// file that may read it, whose reach is asked (main changed nothing else a crate may own) and
/// holds one crate of the branch's two. So do the readers census's guard and a whole-path
/// search miss: a path joined from pieces under a helper's root, a doctest, a
/// `#[doc]`-included README's Rust block, a reader found through a Git that a person's
/// `GIT_LITERAL_PATHSPECS` or `GIT_NOGLOB_PATHSPECS` would leave matching no `*.rs` pathspec,
/// and another member's walk of every member's spec table whose path has two placeholders,
/// which `aterm_census::no_reach::may_read` reads as no table's ([`walks_spec_tables`]).
/// Beside a shared path a crate may own, the reach is asked of that path and the reader
/// together, with main's own change. NEGATIVE CONTROLS: the same reader in a file no
/// workspace member owns (the same tree with no workspace manifest) is in no cone, so it is
/// judged in full, asking no reach; and [`DOC`] where only a comment names it is the first
/// test's round, which names no reader.
#[test]
fn shared_markdown_a_members_file_may_read_is_judged_by_a_round_over_its_readers() {
    let member = [
        ("Cargo.toml", Some(WORKSPACE), false),
        (
            "crates/b/Cargo.toml",
            Some("[package]\nname = \"b\"\n"),
            false,
        ),
    ];
    let joined =
        "pub fn r() -> std::path::PathBuf {\n    root().join(\"docs\").join(\"FIXTURE.md\")\n}\n";
    let doctest = "/// ```\n/// std::fs::read(concat!(env!(\"CARGO_MANIFEST_DIR\"), \
                   \"/../../docs/FIXTURE.md\")).unwrap();\n/// ```\npub fn d() {}\n";
    let readme = "# b\n\n```rust\nstd::fs::read(\"../../docs/FIXTURE.md\").unwrap();\n```\n";
    let own = ("crates/b/SPECS.md", None, false);
    // Each case: the file that may read the Markdown, the files main holds, the shared
    // Markdown, a path a crate may own that both sides change too, and a pathspec mode a
    // person's Git exports.
    type Reading<'a> = (
        &'a str,
        Vec<File<'a>>,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
    );
    let lib = "crates/b/src/lib.rs";
    // A walk of every member's table whose path census's search reads as no table's.
    let walks = "crates/q/src/lib.rs";
    let walk = "pub fn table(root: &str, name: &str) -> String {\n    \
                format!(\"{root}/crates/{name}/SPECS.md\")\n}\n";
    let q = (
        "crates/q/Cargo.toml",
        Some("[package]\nname = \"q\"\n"),
        false,
    );
    let cases: [Reading<'_>; 8] = [
        (
            ROWS,
            vec![own, (ROWS, Some(READS_SPEC), false)],
            SPEC,
            None,
            None,
        ),
        (lib, vec![(lib, Some(joined), false)], DOC, None, None),
        (lib, vec![(lib, Some(doctest), false)], DOC, None, None),
        (
            "crates/b/README.md",
            vec![
                (
                    lib,
                    Some("#![doc = include_str!(\"../README.md\")]\n"),
                    false,
                ),
                ("crates/b/README.md", Some(readme), false),
            ],
            DOC,
            None,
            None,
        ),
        (
            lib,
            vec![(lib, Some(joined), false)],
            DOC,
            None,
            Some("GIT_LITERAL_PATHSPECS"),
        ),
        (
            lib,
            vec![(lib, Some(joined), false)],
            DOC,
            None,
            Some("GIT_NOGLOB_PATHSPECS"),
        ),
        (
            ROWS,
            vec![own, (ROWS, Some(READS_SPEC), false)],
            SPEC,
            Some("shared.txt"),
            None,
        ),
        (
            walks,
            vec![own, q, (walks, Some(walk), false)],
            SPEC,
            None,
            None,
        ),
    ];
    for (reader, files, shared, code, mode) in cases {
        let case = format!("{reader} reads {shared} (beside {code:?}, under {mode:?})");
        let mut all = member.to_vec();
        all.extend(files.iter().copied());
        let world = markdown_world(&all);
        commit(&world.work, "a/x.txt", "a2\n");
        let both: Vec<&str> = [Some(shared), code].into_iter().flatten().collect();
        for file in &both {
            edit_line(&world.work, file, 1, "one");
        }
        let pathspecs = |dir: &Path| {
            let mut command = git_cmd(dir);
            if let Some(mode) = mode {
                command.env(mode, "1");
            }
            command
        };
        let asked = RefCell::new(Vec::new());
        let reach = |paths: Option<&[String]>| match paths {
            None => reaches(&["a", "b"]),
            Some(paths) => {
                asked.borrow_mut().push(paths.to_vec());
                reaches(&["b"])
            }
        };
        let (polls, deltas) = (RefCell::new(Vec::new()), RefCell::new(Vec::new()));
        let (exit, said, bases, rechecks) = land_seamed(
            &world,
            &pathspecs,
            &live_remote,
            &no_retry,
            &reach,
            |_, n, poll| {
                if n == 0 {
                    rival_does(&world, |rival| {
                        for file in &both {
                            edit_line(rival, file, 8, "eight");
                        }
                    });
                    polls.borrow_mut().push(poll(&mut || false));
                }
                Ran::Ended(green())
            },
            |_, delta| {
                deltas.borrow_mut().push(delta.clone());
                ran(
                    exit::PASS,
                    vec![
                        row("fmt", "ok"),
                        row("census", "ok"),
                        row("export-scan", "ok"),
                        row("test", "ok"),
                    ],
                    4,
                )
            },
        );
        assert_eq!(exit, exit::PASS, "{case}: {said:#?}");
        assert_eq!(
            *polls.borrow(),
            [false],
            "{case}: nothing is cut: {said:#?}"
        );
        assert_eq!(
            (bases.len(), rechecks.len()),
            (1, 1),
            "{case}: the judgment stands and one round follows: {said:#?}"
        );
        let mut paths: Vec<String> = both.iter().map(|path| (*path).to_owned()).collect();
        paths.sort();
        let deltas = deltas.into_inner();
        assert_eq!(deltas.len(), 1, "{case}");
        assert_eq!(
            (
                &deltas[0].shared,
                deltas[0].readers.as_slice(),
                deltas[0].mechanical
            ),
            (&paths, &[reader.to_owned()][..], true),
            "{case}"
        );
        // The Markdown stands for its reader; a path a crate may own stands for itself.
        let mut judged: Vec<String> = code.iter().map(|path| (*path).to_owned()).collect();
        judged.push(reader.to_owned());
        judged.sort();
        assert!(
            !asked.borrow().is_empty() && asked.borrow().iter().all(|paths| *paths == judged),
            "{case}: the reach is asked of the reader: {:?}",
            asked.borrow()
        );
        assert!(
            said.iter()
                .any(|line| line
                    .contains(&format!("{reader} may read the Markdown among those paths"))),
            "{case}: {said:#?}"
        );
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        assert_eq!(
            (receipt.checked.len(), receipt.rechecked.len()),
            (1, 1),
            "{case}"
        );
        assert!(receipt.interrupted.is_empty(), "{case}");
        assert!(
            receipt.rechecked[0]
                .rows
                .iter()
                .any(|row| row.check == "test" && row.word == "ok"),
            "{case}: the reader's rows are stated"
        );
    }

    let control = markdown_world(&[(lib, Some(joined), false)]);
    commit(&control.work, "a/x.txt", "a2\n");
    edit_line(&control.work, DOC, 1, "one");
    judged_in_full(
        &control,
        &git_cmd,
        &|rival| drop(edit_line(rival, DOC, 8, "eight")),
        &format!(
            "{DOC} is named in {lib}'s code, so a build or test may read it, and no workspace \
             member owns that file"
        ),
    );
}

/// A RED IN THE ROUND OVER A READER STOPS THE PUSH: the round's delta names the file that may
/// read the merged spec table, and the round's run (scripted here: the selection it makes is
/// [`crate::changed::resolve_round`]'s, tested in `changed_tests`) reports that file's test
/// red; it counts as in a judgment and nothing is pushed. NEGATIVE CONTROL: the same round green
/// lands (the test above).
#[test]
fn a_red_in_the_crate_that_reads_the_merged_markdown_stops_the_push() {
    let rows = "-p b --test rows -- rows";
    let world = spec_world(false, &[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, SPEC, 1, "one");
    let moved = RefCell::new(String::new());
    let (code, said, bases, rechecks) = land_reaching(
        &world,
        &|paths| match paths {
            None => reaches(&["a", "b"]),
            Some(_) => reaches(&["b"]),
        },
        |_, n, _| {
            if n == 0 {
                *moved.borrow_mut() =
                    rival_does(&world, |rival| drop(edit_line(rival, SPEC, 8, "eight")));
            }
            Ran::Ended(green())
        },
        |_, delta| {
            assert_eq!(delta.readers, [ROWS]);
            ran(
                exit::FAILED,
                vec![
                    row("fmt", "ok"),
                    row("census", "ok"),
                    row("export-scan", "ok"),
                    red("test", &[rows]),
                ],
                3,
            )
        },
    );
    assert_eq!(code, exit::FAILED, "{said:#?}");
    assert_eq!((bases.len(), rechecks.len()), (1, 1), "{said:#?}");
    assert_eq!(origin_main(&world), moved.into_inner(), "nothing pushed");
    assert!(
        said.iter()
            .any(|line| line.contains("FAIL — the round of") && line.contains("test")),
        "{said:#?}"
    );
}

/// THE CUT STILL HAPPENS WHERE THE READERS' REACH, WITH MAIN'S CHANGE, HOLDS EVERY CRATE THE
/// BRANCH REACHES, OR NONE OF THEM: the round would judge all of the branch's cone again (a
/// round that leaves nothing of the cone judges all of it), so the running judgment is
/// interrupted at the poll and the merge is judged in full, the interruption naming the files
/// the Markdown stands for. Main's change beside the Markdown is asked with the reader, as
/// for a shared path a crate owns: a reader that reaches one crate of the branch's two cuts
/// where main's own change reaches the other. NEGATIVE CONTROL: the same world where the
/// reader's reach is one crate of the branch's two, and main changed nothing else, is a round
/// and cuts nothing.
#[test]
fn readers_that_reach_every_crate_the_branch_reaches_cut_the_judgment() {
    let main_own = "a/z.txt";
    let every = "reaches every crate the branch's change reaches (a, b)";
    let none = "reaches none of the crates the branch's change reaches (a, b), so a round would \
                judge all of them again";
    for (reader_reach, own, cut) in [
        (&["a", "b", "c"][..], false, Some(every)),
        (&["c"][..], false, Some(none)),
        (&["b"][..], true, Some(every)),
        (&["b"][..], false, None),
    ] {
        let world = spec_world(false, &[]);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, SPEC, 1, "one");
        let (code, said, bases, rechecks) = land_reaching(
            &world,
            &|paths| match paths {
                None => reaches(&["a", "b"]),
                Some(paths) if own => {
                    assert_eq!(paths, [main_own, ROWS]);
                    reaches(&["a", "b"])
                }
                Some(paths) => {
                    assert_eq!(paths, [ROWS]);
                    reaches(reader_reach)
                }
            },
            |_, n, poll| {
                if n > 0 {
                    return Ran::Ended(green());
                }
                rival_does(&world, |rival| {
                    if own {
                        commit(rival, main_own, "main's own\n");
                    }
                    drop(edit_line(rival, SPEC, 8, "eight"));
                });
                let cut = cut.is_some();
                if poll(&mut || false) {
                    assert!(cut, "a reach narrower than the branch's cuts nothing");
                    Ran::Interrupted {
                        secs: 1,
                        queued: false,
                    }
                } else {
                    assert!(!cut, "a reach that holds the branch's cuts");
                    Ran::Ended(green())
                }
            },
            |_, _| rechecked(),
        );
        assert_eq!(code, exit::PASS, "{said:#?}");
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        let Some(why) = cut else {
            assert_eq!((bases.len(), rechecks.len()), (1, 1), "{said:#?}");
            assert!(receipt.interrupted.is_empty());
            continue;
        };
        assert_eq!((bases.len(), rechecks.len()), (2, 0), "{said:#?}");
        assert_eq!(receipt.interrupted.len(), 1);
        assert!(
            said.iter().any(|line| line.contains("INTERRUPTED")
                && line.contains(why)
                && line.ends_with(&format!(
                    "the shared Markdown stands for the files that may read it ({ROWS})"
                ))),
            "{said:#?}"
        );
    }
}

/// MARKDOWN THAT IS NO REGULAR FILE ON EVERY SIDE IS STILL JUDGED IN FULL, whoever may read it:
/// a round over its readers answers for an edit of a file both sides hold, not for a mode or a
/// path that moved. An executable spec table, and one the branch or main renames, each with a
/// member's test that reads it, are judged in full and nothing is rechecked. The control is
/// [`shared_markdown_a_members_file_may_read_is_judged_by_a_round_over_its_readers`], where
/// the same table is a regular file on every side.
#[test]
fn markdown_a_file_may_read_that_is_no_regular_file_on_every_side_is_judged_in_full() {
    let why = format!("{SPEC} is not one regular file in");
    let world = spec_world(true, &[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, SPEC, 1, "one");
    judged_in_full(
        &world,
        &git_cmd,
        &|rival| drop(edit_line(rival, SPEC, 8, "eight")),
        &why,
    );

    let moved = "crates/b/docs/SPECS.md";
    let world = spec_world(false, &[]);
    commit(&world.work, "a/x.txt", "a2\n");
    move_into(&world.work, SPEC, moved, &[]);
    judged_in_full(
        &world,
        &git_cmd,
        &|rival| drop(edit_line(rival, SPEC, 8, "eight")),
        &why,
    );

    let world = spec_world(false, &[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, SPEC, 1, "one");
    judged_in_full(
        &world,
        &git_cmd,
        &|rival| move_into(rival, SPEC, moved, &[]),
        &why,
    );
}

/// R35: a message string that cites the Markdown for a person, `(docs/X.md § Resume)`, reads
/// nothing, so the shared Markdown is the source readers' alone and a round of fmt, census and
/// export-scan answers for it (aterm-release cites its guide so, and every landing that shared
/// it judged aterm-release and its dependents again). NEGATIVE CONTROLS: an `include_str!` of
/// it, a `Path::new` of it, a const later joined into a path, a test that joins its file name
/// under a root, and prose that spells it bare each make their file a reader; in no workspace
/// member, as here, the merge is judged in full.
#[test]
fn a_message_that_cites_the_markdown_composes_and_a_read_of_it_is_judged_in_full() {
    let cites = "pub fn why() -> &'static str {\n    \"resume the cut (docs/FIXTURE.md \u{a7} \
                 Resume), then land\"\n}\n";
    let world = markdown_world(&[("crates/b/src/lib.rs", Some(cites), false)]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    let (code, said, bases, rechecks) = land_rechecking(
        &world,
        &git_cmd,
        crates(&["a"]),
        |_, n, _| {
            if n == 0 {
                rival_does(&world, |rival| drop(edit_line(rival, DOC, 8, "eight")));
            }
            Ran::Ended(green())
        },
        |_| rechecked(),
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "rechecked, not judged again: {said:#?}");
    assert_eq!(rechecks.len(), 1, "{said:#?}");

    let reads = [
        (
            "crates/b/src/lib.rs",
            "pub const G: &str = include_str!(\"../../../docs/FIXTURE.md\");\n",
        ),
        (
            "crates/b/src/lib.rs",
            "pub fn p() -> &'static std::path::Path {\n    \
             std::path::Path::new(\"docs/FIXTURE.md\")\n}\n",
        ),
        (
            "crates/b/src/lib.rs",
            "const GUIDE: &str = \"docs/FIXTURE.md\";\n\
             pub fn p() -> std::path::PathBuf {\n    root().join(GUIDE)\n}\n",
        ),
        (
            "crates/b/tests/guide.rs",
            "#[test]\nfn guide() {\n    \
             std::fs::read_to_string(root().join(\"docs\").join(\"FIXTURE.md\")).unwrap();\n}\n",
        ),
        (
            "crates/b/src/lib.rs",
            "pub fn why() -> &'static str {\n    \"see docs/FIXTURE.md for the steps\"\n}\n",
        ),
    ];
    for (reader, text) in reads {
        let world = markdown_world(&[(reader, Some(text), false)]);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, DOC, 1, "one");
        judged_in_full(
            &world,
            &git_cmd,
            &|rival| drop(edit_line(rival, DOC, 8, "eight")),
            &format!("{DOC} is named in {reader}'s code"),
        );
    }
}

/// The Markdown landings share most composes on this checkout: no file outside census may read
/// `docs/KNOWN_ISSUES.md`, `docs/RELEASING.md` (which nearly every release fix keeps true, R35)
/// or `docs/PROCESS.md` by the land-time search ([`reads`]), so a test fixture or helper that
/// spells one in code (whose crate every overlap of it would then be judged for) fails here; a
/// message may cite one as `(docs/RELEASING.md § …)`. Each path is spelled in two pieces, as it
/// was while this file was itself searched. And no file of this crate reads a member's spec
/// table or a tracker document by that search: its landing and cone rule sources are passed
/// over ([`rule_source`]), and no other file of it spells one in code, so a landing that shares
/// one with main is not judged again for this crate.
#[test]
fn the_live_tree_names_the_composing_markdown_in_no_code() {
    let paths = [
        concat!("docs/KNOWN", "_ISSUES.md"),
        concat!("docs/RELEAS", "ING.md"),
        concat!("docs/PROC", "ESS.md"),
    ];
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
            {
                for path in paths {
                    if reads(&rel, &text, path) {
                        readers.push(format!("{rel} may read {path}"));
                    }
                }
                if rel.starts_with("crates/aterm-verify/") {
                    for path in [
                        "crates/aterm-gui/SPECS.md",
                        "docs/PLAN.md",
                        "docs/STATUS.md",
                    ] {
                        if reads(&rel, &text, path) {
                            readers.push(format!("{rel} may read {path}"));
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        readers,
        Vec::<String>::new(),
        "each of these spells a path in code, so every landing that shares it judges its crate again"
    );
}

/// A RESUMED ROUND OF SHARED MARKDOWN LANDS AS ONE: a landing whose round judged Markdown alone
/// (main moved [`DOC`]'s last line while the branch's judgment ran) and whose every push then
/// met a main that moved as it pushed, until the bound stopped it, keeps that round; run again, it
/// judges nothing, composes onto the moved main and lands, its end record naming the landing
/// `markdown`, resumed, as the run that judged the round would have named it. NEGATIVE CONTROL,
/// run once as a mutant that made this test fail: `take_resumed` leaving the resumed round's
/// Markdown unread (the landing was named `rechecked`).
#[test]
fn a_resumed_round_of_shared_markdown_lands_as_markdown() {
    let world = markdown_world(&[]);
    commit(&world.work, "a/x.txt", "a2\n");
    edit_line(&world.work, DOC, 1, "one");
    let moves = std::cell::Cell::new(0);
    let racing: &RemoteStep<'_> = &|root, git, tool, args, until, mode| {
        if args.iter().any(|word| word == "push") {
            moves.set(moves.get() + 1);
            rival_moves(&world, "b/y.txt", &format!("b moved {}\n", moves.get()));
        }
        live_remote(root, git, tool, args, until, mode)
    };
    let (code, said, _, rounds) = land_wired(
        &world,
        &git_cmd,
        racing,
        &no_retry,
        crates(&["a"]),
        |_, n, _| {
            if n == 0 {
                rival_does(&world, |rival| drop(edit_line(rival, DOC, 8, "eight")));
            }
            Ran::Ended(green())
        },
        |_| rechecked(),
    );
    assert_eq!(code, STOPPED, "{said:#?}");
    assert_eq!(rounds.len(), 1, "{said:#?}");
    let (code, said, bases, rounds) = land_rechecking(
        &world,
        &git_cmd,
        crates(&["a"]),
        |base, _, _| panic!("nothing is judged again (base {base})"),
        |base| panic!("no round runs again (base {base})"),
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert!(bases.is_empty() && rounds.is_empty(), "{said:#?}");
    let told = super::quietly();
    let [_, (_, landing)] = told.as_slice() else {
        panic!("two runs: {told:#?}");
    };
    assert_eq!(landing, &Some(("markdown".to_owned(), true)), "{told:#?}");
}
