// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUSH OF A PASSED JUDGMENT: a main that keeps moving is composed onto within the bound,
//! each remote step under its own hang detector and no deadline across them; a remote step that
//! fails or times out having sent nothing is made again, and a push whose outcome is unknown
//! never is; and a judgment that passed is kept, so the next run resumes at the push while HEAD
//! is the judged commit. Each test sits beside the control that shows the rule bites.

use super::*;
use std::cell::Cell;
use std::ffi::OsString;

/// What a remote step the landing's own hang detector ended reports.
const TIMED_OUT: &str = "Git remote operation: did not finish in time";

/// How much of a hang detector a step in these tests may have used by the time the seam reads
/// it: the tests run in well under this.
const SLACK: Duration = aterm_spec::wait::MUST_HAPPEN;

/// Whether a remote step is the landing's push to main.
fn is_push(args: &[OsString]) -> bool {
    args.iter().any(|word| word == "push")
}

/// Whether a remote step is a read of main's tip.
fn is_read(args: &[OsString]) -> bool {
    args.iter().any(|word| word == "ls-remote")
}

/// The commit [`KEPT`] names in `world`'s work checkout, if any.
fn kept_record(world: &World) -> Option<String> {
    let mut cmd = git_cmd(&world.work);
    cmd.args(["rev-parse", "-q", "--verify", KEPT]);
    let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
        .expect("git rev-parse");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn head_tree(world: &World) -> String {
    git(&world.work, &["rev-parse", "HEAD^{tree}"])
}

/// What one landing over [`racing`]'s remote came to.
struct Raced {
    code: i32,
    said: Vec<String>,
    /// Each judged base.
    bases: Vec<String>,
    /// How many pushes the landing made.
    pushes: usize,
    /// Each tip the rival moved main to, in order.
    tips: Vec<String>,
}

/// Land `world`'s branch, judged green, over a remote whose main the rival moves (a file the
/// branch does not touch) just before each of the landing's first `moves` pushes. Every remote
/// step must come with its own whole hang detector ([`WRITE`] for a push, [`NETWORK`] for any
/// other), however many steps and landings came before it: a deadline shared across them, as
/// the one that ended the measured landing of 2026-11-12 at its seventh push, fails here. The
/// judgment must already be kept when the first push starts.
fn racing(world: &World, moves: usize) -> Raced {
    let left = Cell::new(moves);
    let pushes = Cell::new(0);
    let tips = RefCell::new(Vec::new());
    let step: &RemoteStep<'_> = &|root, git, tool, args, until| {
        let allowed = until.saturating_duration_since(Instant::now());
        if is_push(args) {
            assert!(
                allowed > WRITE - SLACK,
                "push {}: {allowed:?}",
                pushes.get()
            );
            if pushes.get() == 0 {
                assert!(kept_record(world).is_some(), "kept before the first push");
            }
            pushes.set(pushes.get() + 1);
            if left.get() > 0 {
                left.set(left.get() - 1);
                let text = format!("b moved, {} to go\n", left.get());
                tips.borrow_mut().push(rival_moves(world, "b/y.txt", &text));
            }
        } else {
            assert!(
                allowed > NETWORK - SLACK,
                "{args:?} after {} pushes: {allowed:?}",
                pushes.get()
            );
        }
        crate::remote::run(root, git, tool, args, until)
    };
    let (code, said, bases, _) = land_wired(
        world,
        &git_cmd,
        step,
        &no_retry,
        crates(&["a"]),
        Lane::Pass,
        |_, _, _| Ran::Ended(green()),
        |base| panic!("no recheck was expected (base {base})"),
    );
    Raced {
        code,
        said,
        bases,
        pushes: pushes.get(),
        tips: tips.take(),
    }
}

/// A MAIN THAT MOVES BETWEEN THE JUDGMENT AND EVERY PUSH IS COMPOSED ONTO EACH TIME, AND THE ONE
/// JUDGMENT LANDS: main moves before each of [`COMPOSES`] pushes in a row (the measured landing
/// lost six), the judgment runs once, and the next push lands it composed onto the last tip.
/// NEGATIVE CONTROL: one more move stops the landing at the stated bound, nothing pushed, its
/// judgment kept.
#[test]
fn a_main_that_moves_before_every_push_is_composed_onto_and_lands_within_the_bound() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    let judged = head_tree(&world);
    let raced = racing(&world, COMPOSES);
    assert_eq!(raced.code, exit::PASS, "{:#?}", raced.said);
    assert_eq!(raced.bases.len(), 1, "judged once: {:#?}", raced.said);
    assert_eq!(raced.pushes, COMPOSES + 1);
    let last = raced.tips.last().expect("main moved");
    let landing = origin_main(&world);
    assert_eq!(
        git(&world.work, &["rev-parse", &format!("{landing}^2")]),
        *last
    );
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(receipt.composed_onto.as_deref(), Some(last.as_str()));
    assert_eq!(receipt.checked.len(), 1);
    assert_eq!(receipt.checked[0].tree, judged);
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:b/y.txt")]),
        "b moved, 0 to go"
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:a/x.txt")]),
        "a2"
    );
    assert_eq!(kept_record(&world), None, "a landed judgment is forgotten");

    let control = self::world();
    commit(&control.work, "a/x.txt", "a2\n");
    let raced = racing(&control, COMPOSES + 1);
    assert_eq!(raced.code, STOPPED, "{:#?}", raced.said);
    assert_eq!((raced.bases.len(), raced.pushes), (1, COMPOSES + 1));
    assert_eq!(origin_main(&control), *raced.tips.last().expect("moved"));
    assert!(
        raced
            .said
            .iter()
            .any(|line| line.contains(&format!("STOPPED after {COMPOSES} push/composition races"))),
        "{:#?}",
        raced.said
    );
    assert!(kept_record(&control).is_some(), "{:#?}", raced.said);
}

/// A landing of `world`'s branch that passed and stopped at the composition bound with nothing
/// pushed ([`racing`]). Main had moved once before it, so the judged commit is the tool's own
/// merge. The judged commit, its tree and the main commit it merged.
fn stopped(world: &World) -> (String, String, String) {
    let merged = rival_moves(world, "b/y.txt", "b before the run\n");
    let raced = racing(world, COMPOSES + 1);
    assert_eq!(raced.code, STOPPED, "{:#?}", raced.said);
    assert_eq!(
        raced.bases,
        std::slice::from_ref(&merged),
        "{:#?}",
        raced.said
    );
    assert!(
        raced
            .said
            .iter()
            .any(|line| line.contains("is kept: run it again")),
        "{:#?}",
        raced.said
    );
    let judged = git(&world.work, &["rev-parse", "HEAD"]);
    assert_eq!(
        git(&world.work, &["rev-parse", "HEAD^2"]),
        merged,
        "HEAD is the judged merge"
    );
    (judged, head_tree(world), merged)
}

/// How the checkout stands when a stopped landing is run again.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Rerun {
    /// As the earlier run left it.
    Same,
    /// HEAD reworded: the judged tree under another commit.
    Reworded,
    /// The kept record gone.
    Forgotten,
}

/// THE NEXT RUN OF A LANDING THAT STOPPED AFTER ITS JUDGMENT PASSED GOES STRAIGHT TO THE PUSH:
/// while HEAD is the judged commit nothing is judged again, the moved main is composed onto by
/// the same rule, and the landing states the judgment the earlier run made. NEGATIVE CONTROLS:
/// a head reworded since (the same tree under another commit) is judged again, and so is the
/// same head once its record is gone: the record alone carries the pass.
#[test]
fn a_stopped_landing_resumes_at_the_push_and_judges_nothing_while_head_is_the_judged_commit() {
    for rerun in [Rerun::Same, Rerun::Reworded, Rerun::Forgotten] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        let (judged, tree, merged) = stopped(&world);
        match rerun {
            Rerun::Same => {}
            Rerun::Reworded => {
                git(
                    &world.work,
                    &[
                        "commit",
                        "-q",
                        "--amend",
                        "-m",
                        "the judged merge, reworded",
                    ],
                );
            }
            Rerun::Forgotten => {
                git(&world.work, &["update-ref", "-d", KEPT]);
            }
        }
        if rerun != Rerun::Same {
            let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
            assert_eq!((code, bases.len()), (exit::PASS, 1), "{rerun:?} {said:#?}");
            assert_eq!(
                said.iter()
                    .any(|line| line.contains("the kept judgment is not resumed")),
                rerun == Rerun::Reworded,
                "{said:#?}"
            );
            assert!(
                !said
                    .iter()
                    .any(|line| line.contains("resuming at the push")),
                "{said:#?}"
            );
            continue;
        }
        let tip = rival_moves(&world, "b/y.txt", "b between the runs\n");
        let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |base, _| {
            panic!("a kept judgment is not made again (base {base})")
        });
        assert_eq!(code, exit::PASS, "{said:#?}");
        assert!(bases.is_empty(), "{said:#?}");
        assert!(
            said.iter()
                .any(|line| line.contains("verify land: resuming at the push")),
            "{said:#?}"
        );
        assert!(
            !said.iter().any(|line| line.contains("the branch's crates")),
            "a resumed landing that judges nothing reads no cone: {said:#?}"
        );
        let landing = origin_main(&world);
        let parents = git(&world.work, &["rev-parse", &format!("{landing}^@")]);
        assert_eq!(
            parents.lines().collect::<Vec<_>>(),
            [judged.as_str(), tip.as_str()]
        );
        let receipt = stated(&world, &landing).expect("stated");
        assert_eq!(
            receipt.checked,
            [Checked {
                tree: tree.clone(),
                rows: green()
                    .checks
                    .iter()
                    .map(|row| Row {
                        check: row.check.clone(),
                        word: row.word.clone(),
                    })
                    .collect(),
                base: merged.clone(),
                secs: green().secs,
            }],
            "the earlier run's judgment, as it was made"
        );
        assert_eq!(receipt.composed_onto.as_deref(), Some(tip.as_str()));
        assert_eq!(receipt.rejudges(), 0);
        assert_eq!(
            git(&world.work, &["show", &format!("{landing}:b/y.txt")]),
            "b between the runs"
        );
        assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
        assert_eq!(kept_record(&world), None);
    }
}

/// A KEPT JUDGMENT IS NEVER CARRIED TO ANOTHER TREE, NOR PAST A MAIN THAT DOES NOT COMPOSE.
/// NEGATIVE CONTROLS of the resume: a head that gained a commit is judged, as is one whose
/// record was moved to name the new tree under the old receipt; and a main that changed a file
/// the branch changed is merged and judged again, the landing stating both judgments.
#[test]
fn a_kept_judgment_is_never_carried_to_another_tree_or_past_a_main_that_does_not_compose() {
    for forged in [false, true] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        let (_, tree, merged) = stopped(&world);
        let record = kept_record(&world).expect("kept");
        commit(&world.work, "a/x.txt", "a3\n");
        let changed = head_tree(&world);
        assert_ne!(changed, tree);
        if forged {
            let message = git(&world.work, &["log", "-1", "--format=%B", &record]);
            let moved = git(
                &world.work,
                &[
                    "commit-tree",
                    &changed,
                    "-p",
                    "HEAD",
                    "-p",
                    &merged,
                    "-m",
                    &message,
                ],
            );
            git(&world.work, &["update-ref", KEPT, &moved]);
        }
        let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
        assert_eq!(code, exit::PASS, "{said:#?}");
        assert_eq!(bases.len(), 1, "judged: {said:#?}");
        assert!(
            said.iter()
                .any(|line| line.contains("the kept judgment is not resumed")),
            "{said:#?}"
        );
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        assert_eq!(receipt.checked.len(), 1);
        assert_ne!(receipt.checked[0].tree, tree, "{said:#?}");
    }

    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let (_, tree, merged) = stopped(&world);
    let tip = rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
    let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |_, _| green());
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases, std::slice::from_ref(&tip), "judged again: {said:#?}");
    assert!(
        said.iter().any(|line| line.contains("does not compose")),
        "{said:#?}"
    );
    let landing = origin_main(&world);
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(receipt.checked.len(), 2);
    assert_eq!(
        (
            receipt.checked[0].tree.as_str(),
            receipt.checked[0].base.as_str()
        ),
        (tree.as_str(), merged.as_str())
    );
    assert_eq!(receipt.checked[1].base, tip);
    assert_eq!(receipt.composed_onto, None);
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:shared.txt")]),
        "one\n2\n3\n4\n5\n6\n7\neight"
    );
}

/// A READ OF MAIN THAT TIMES OUT AFTER THE JUDGMENT PASSED IS MADE AGAIN, AFTER A DOUBLING
/// PAUSE, AND THE LANDING LANDS: main moved before the push, and the reads of where it went
/// fail. NEGATIVE CONTROL: one failure more than [`RETRIES`] in a row stops the landing with
/// nothing pushed and its judgment kept, and the next run lands it without judging.
#[test]
fn a_read_of_main_that_times_out_after_the_pass_is_made_again_and_the_landing_lands() {
    for failures in [1, RETRIES, RETRIES + 1] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        let tree = head_tree(&world);
        let moved = RefCell::new(None);
        let failed = Cell::new(0);
        let pauses = RefCell::new(Vec::new());
        let step: &RemoteStep<'_> = &|root, git, tool, args, until| {
            if is_push(args) && moved.borrow().is_none() {
                *moved.borrow_mut() = Some(rival_moves(&world, "b/y.txt", "b2\n"));
            }
            if is_read(args) && moved.borrow().is_some() && failed.get() < failures {
                failed.set(failed.get() + 1);
                return Err(TIMED_OUT.to_owned());
            }
            crate::remote::run(root, git, tool, args, until)
        };
        let (code, said, bases, _) = land_wired(
            &world,
            &git_cmd,
            step,
            &|pause| pauses.borrow_mut().push(pause),
            crates(&["a"]),
            Lane::Pass,
            |_, _, _| Ran::Ended(green()),
            |base| panic!("no recheck was expected (base {base})"),
        );
        let tip = moved.take().expect("main moved");
        let made_again = failures.min(RETRIES);
        let doubling: Vec<Duration> = (0..made_again).map(|n| RETRY_PAUSE * (1u32 << n)).collect();
        assert_eq!(*pauses.borrow(), doubling, "{said:#?}");
        assert_eq!(bases.len(), 1, "{said:#?}");
        if failures <= RETRIES {
            assert_eq!(code, exit::PASS, "{said:#?}");
        } else {
            assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
            assert_eq!(origin_main(&world), tip, "nothing was pushed");
            let last = said.last().expect("said");
            assert!(
                last.contains(TIMED_OUT)
                    && last.contains(&format!("stopped after {RETRIES} failures"))
                    && last.contains("is kept: run it again"),
                "{said:#?}"
            );
            let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |base, _| {
                panic!("a kept judgment is not made again (base {base})")
            });
            assert_eq!(code, exit::PASS, "{said:#?}");
            assert!(bases.is_empty(), "{said:#?}");
        }
        let landing = origin_main(&world);
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{landing}^2")]),
            tip
        );
        let receipt = stated(&world, &landing).expect("stated");
        assert_eq!(receipt.checked.len(), 1);
        assert_eq!(receipt.checked[0].tree, tree);
        assert_eq!(receipt.composed_onto.as_deref(), Some(tip.as_str()));
    }
}

/// A PUSH THAT ENDED WITH NO ANSWER IS NEVER MADE AGAIN, AND A MAIN THAT HOLDS ITS LANDING TOOK
/// IT: the push's report is lost to a timeout. Where the push had reached main, the landing is
/// landed. NEGATIVE CONTROL: where it had not, the landing stops after that one push with its
/// judgment kept, and the next run pushes it without judging.
#[test]
fn a_push_that_timed_out_is_landed_where_main_took_it_and_never_made_again() {
    for sent in [true, false] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        let tree = head_tree(&world);
        let before = origin_main(&world);
        let pushes = Cell::new(0);
        let step: &RemoteStep<'_> = &|root, git, tool, args, until| {
            if is_push(args) {
                pushes.set(pushes.get() + 1);
                if sent {
                    let out = crate::remote::run(root, git, tool, args, until)?;
                    assert!(out.status.success(), "the push reached main");
                }
                return Err(TIMED_OUT.to_owned());
            }
            crate::remote::run(root, git, tool, args, until)
        };
        let (code, said, bases, _) = land_wired(
            &world,
            &git_cmd,
            step,
            &no_retry,
            crates(&["a"]),
            Lane::Pass,
            |_, _, _| Ran::Ended(green()),
            |base| panic!("no recheck was expected (base {base})"),
        );
        assert_eq!((bases.len(), pushes.get()), (1, 1), "{said:#?}");
        if sent {
            assert_eq!(code, exit::PASS, "{said:#?}");
            assert!(
                said.iter().any(|line| line.contains("verify land: LANDED")),
                "{said:#?}"
            );
            assert_eq!(kept_record(&world), None);
        } else {
            assert_eq!(code, exit::COULD_NOT_RUN, "{said:#?}");
            assert_eq!(origin_main(&world), before, "nothing was pushed");
            let last = said.last().expect("said");
            assert!(
                last.contains(TIMED_OUT)
                    && last.contains("a push whose outcome is unknown is not made again")
                    && last.contains("is kept, but the write of")
                    && last.contains("settle that before running it again"),
                "{said:#?}"
            );
            let (code, said, bases) = land_with(&world, crates(&["a"]), Lane::Pass, |base, _| {
                panic!("a kept judgment is not made again (base {base})")
            });
            assert_eq!(code, exit::PASS, "{said:#?}");
            assert!(bases.is_empty(), "{said:#?}");
        }
        let landing = origin_main(&world);
        let receipt = stated(&world, &landing).expect("stated");
        assert_eq!(receipt.checked.len(), 1);
        assert_eq!(receipt.checked[0].tree, tree);
        assert_eq!(receipt.composed_onto, None);
        assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing);
    }
}
