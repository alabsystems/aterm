// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUSH OF A PASSED JUDGMENT: a main that keeps moving is composed onto within the bound,
//! each remote step under its own hang detector and no deadline across them; a remote step that
//! fails or times out having sent nothing is made again, and a push whose outcome is unknown
//! never is; and a judgment that passed is kept, so the next run resumes at the push while HEAD
//! is the judged commit. A main that conflicts with the branch keeps the judgment too: the
//! merge a person commits is judged by a round over the paths it shares, or in full where no
//! round answers for it. Each test sits beside the control that shows the rule bites.

use super::markdown::{DOC, edit_line, markdown_world, rechecked, rival_does};
use super::*;
use aterm_types::land_trailer::Rechecked;
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

/// A tracking ref replaced by a symbolic alias must never redirect the bookkeeping write
/// into that alias's target branch, even when the target has the expected old object ID.
#[test]
fn tracking_a_landing_never_writes_a_symbolic_refs_target_branch() {
    for raced in [false, true] {
        let world = world();
        let onto = git(&world.work, &["rev-parse", "origin/main"]);
        let landing = commit(&world.work, "a/x.txt", "a2\n");
        git(&world.work, &["update-ref", "refs/heads/other", &onto]);
        let alias = || {
            git(
                &world.work,
                &[
                    "symbolic-ref",
                    "refs/remotes/origin/main",
                    "refs/heads/other",
                ],
            );
        };
        if !raced {
            alias();
        }
        let calls = Cell::new(0);
        let make = |root: &Path| {
            calls.set(calls.get() + 1);
            if raced && calls.get() == 2 {
                alias();
            }
            git_cmd(root)
        };
        let plumbing = Git {
            root: &world.work,
            make: &make,
        };
        plumbing
            .track_landing(&landing, &onto)
            .expect("tracking write");
        assert_eq!(
            git(&world.work, &["rev-parse", "refs/heads/other"]),
            onto,
            "raced={raced}"
        );
        assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), landing);
    }
}

#[test]
fn fetching_main_never_writes_a_symbolic_refs_target_branch() {
    let world = world();
    let onto = git(&world.work, &["rev-parse", "origin/main"]);
    git(&world.work, &["update-ref", "refs/heads/other", &onto]);
    let tip = rival_moves(&world, "b/y.txt", "new main\n");
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let fetched = plumbing
        .fetch_through(&|tool, args| {
            let out =
                crate::remote::run(&world.work, &git_cmd, tool, args, Instant::now() + SLACK)?;
            if args.iter().any(|arg| arg == "fetch") && out.status.success() {
                git(
                    &world.work,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/main",
                        "refs/heads/other",
                    ],
                );
            }
            Ok(out)
        })
        .expect("fetch main");
    assert_eq!(fetched, tip);
    assert_eq!(git(&world.work, &["rev-parse", "refs/heads/other"]), onto);
    assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), tip);
}

#[test]
fn fetching_main_cannot_overwrite_an_observation_made_during_its_read() {
    let world = world();
    let tip = rival_moves(&world, "b/y.txt", "main at read\n");
    let newer = commit(&world.rival, "b/y.txt", "observed later\n");
    let plumbing = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let fetched = plumbing.fetch_through(&|tool, args| {
        let out = crate::remote::run(&world.work, &git_cmd, tool, args, Instant::now() + SLACK)?;
        if args.iter().any(|arg| arg == "fetch") && out.status.success() {
            git(&world.rival, &["push", "-q", "origin", "main"]);
            git(&world.work, &["fetch", "-q", path(&world.rival), &newer]);
            git(
                &world.work,
                &["update-ref", "refs/remotes/origin/main", &newer],
            );
        }
        Ok(out)
    });
    assert!(
        fetched.is_err(),
        "the raced cache update must be refused, not overwrite {newer} with {tip}"
    );
    assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), newer);
}

#[test]
fn fetching_main_creates_an_absent_cache_or_accepts_the_same_observation() {
    for absent in [false, true] {
        let world = world();
        let tip = rival_moves(&world, "b/y.txt", "new main\n");
        git(
            &world.work,
            &["config", "--unset-all", "remote.origin.fetch"],
        );
        if absent {
            git(
                &world.work,
                &["update-ref", "-d", "refs/remotes/origin/main"],
            );
        }
        let plumbing = Git {
            root: &world.work,
            make: &git_cmd,
        };
        let fetched = plumbing
            .fetch_through(&|tool, args| {
                let out =
                    crate::remote::run(&world.work, &git_cmd, tool, args, Instant::now() + SLACK)?;
                if !absent && args.iter().any(|arg| arg == "fetch") && out.status.success() {
                    git(
                        &world.work,
                        &["update-ref", "refs/remotes/origin/main", &tip],
                    );
                }
                Ok(out)
            })
            .expect("fetch main");
        assert_eq!(fetched, tip);
        assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), tip);
    }
}

#[test]
fn tracking_cas_cannot_overwrite_an_observation_after_its_read() {
    let world = world();
    let onto = git(&world.work, &["rev-parse", "origin/main"]);
    let landing = commit(&world.work, "a/x.txt", "a2\n");
    let newer = commit(&world.work, "b/y.txt", "b2\n");
    let calls = Cell::new(0);
    let make = |root: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            git(root, &["update-ref", "refs/remotes/origin/main", &newer]);
        }
        git_cmd(root)
    };
    let plumbing = Git {
        root: &world.work,
        make: &make,
    };
    assert!(plumbing.track_landing(&landing, &onto).is_err());
    assert_eq!(calls.get(), 2, "the writer raced the read and CAS");
    assert_eq!(git(&world.work, &["rev-parse", "origin/main"]), newer);
}

/// Tracking metadata cannot turn a successful remote write into another push. A concurrent
/// observer's newer tip survives; a locked ref is reported without losing the landing.
#[test]
fn tracking_after_a_push_preserves_a_newer_tip_and_tolerates_a_locked_ref() {
    for locked in [false, true] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        git(
            &world.work,
            &["config", "--unset-all", "remote.origin.fetch"],
        );
        let pushes = Cell::new(0);
        let observed = RefCell::new(None);
        let step: &RemoteStep<'_> = &|root, make, tool, args, until| {
            let out = crate::remote::run(root, make, tool, args, until)?;
            if is_push(args) && out.status.success() {
                pushes.set(pushes.get() + 1);
                if locked {
                    std::fs::write(
                        world.work.join(".git/refs/remotes/origin/main.lock"),
                        b"another writer\n",
                    )
                    .expect("lock tracking ref");
                } else {
                    let tip = rival_moves(&world, "b/y.txt", "observed after landing\n");
                    git(&world.work, &["fetch", "-q", "origin", &tip]);
                    git(
                        &world.work,
                        &["update-ref", "refs/remotes/origin/main", &tip],
                    );
                    *observed.borrow_mut() = Some(tip);
                }
            }
            Ok(out)
        };
        let (code, said, _, _) = land_wired(
            &world,
            &git_cmd,
            step,
            &|_| {},
            crates(&["a"]),
            Lane::Pass,
            |_, _, _| Ran::Ended(green()),
            |_| panic!("no recheck"),
        );
        assert_eq!(code, exit::PASS, "{said:#?}");
        assert_eq!(pushes.get(), 1, "no replay of the successful push");
        let landing = git(&world.work, &["rev-parse", "HEAD"]);
        assert_eq!(git(&world.work, &["status", "--porcelain"]), "");
        if locked {
            assert_eq!(origin_main(&world), landing);
            assert!(
                said.iter()
                    .any(|line| line.contains("local tracking ref was not refreshed")),
                "{said:#?}"
            );
        } else {
            assert_eq!(
                git(&world.work, &["rev-parse", "origin/main"]),
                observed.borrow().as_deref().unwrap()
            );
            assert_eq!(
                git(&world.work, &["merge-base", &landing, &origin_main(&world)]),
                landing
            );
        }
    }
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

/// The reach of a landing whose shared paths reach less than the branch's change does: a tip
/// that shares a path a crate may own is answered by a round, and one that conflicts in such a
/// path keeps the judgment.
fn narrow(paths: Option<&[String]>) -> Reach {
    match paths {
        None => reaches(&["a", "b"]),
        Some(_) => reaches(&["b"]),
    }
}

/// A scripted reach ([`land_reaching`]).
type Reaching<'a> = &'a dyn Fn(Option<&[String]>) -> Reach;

/// A hand merge: its name, the text it commits at each file, and the paths it shares.
type Resolved<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a [&'a str]);

/// A round that ran the source readers and its cone's tests, all passing.
fn round_green() -> Judged {
    ran(
        exit::PASS,
        vec![
            row("fmt", "ok"),
            row("census", "ok"),
            row("export-scan", "ok"),
            row("test", "ok"),
        ],
        5,
    )
}

/// Land `world`'s branch, judged green, while main makes `rival`'s commits during the
/// judgment, which conflict with the branch: the judgment finishes (polled after the move
/// where `polls`, and not interrupted), the landing stops on the conflict with nothing pushed,
/// and the judgment is kept for the merge the person commits. The judged commit, the main it
/// merged and main's conflicting tip.
fn stopped_on_conflict(
    world: &World,
    polls: bool,
    rival: &dyn Fn(&Path),
) -> (String, String, String) {
    let (judged, merged, tip, _) = stopped_on_conflict_saying(world, polls, rival);
    (judged, merged, tip)
}

/// [`stopped_on_conflict`], and what the landing said.
fn stopped_on_conflict_saying(
    world: &World,
    polls: bool,
    rival: &dyn Fn(&Path),
) -> (String, String, String, Vec<String>) {
    let judged = git(&world.work, &["rev-parse", "HEAD"]);
    let merged = origin_main(world);
    let tip = RefCell::new(String::new());
    let (code, said, bases, _) = land_reaching(
        world,
        crates(&["a"]),
        Lane::Pass,
        &narrow,
        |_, _, poll| {
            *tip.borrow_mut() = rival_does(world, rival);
            if polls {
                assert!(!poll(&mut || false), "a conflicting tip cuts nothing");
            }
            Ran::Ended(green())
        },
        |base, _| panic!("a conflicting tip is answered by no round (base {base})"),
    );
    let tip = tip.into_inner();
    assert_eq!((code, bases.len()), (exit::CONFLICT, 1), "{said:#?}");
    assert!(
        said.iter()
            .any(|line| line.contains("CONFLICT — merging") && line.contains("conflicts in")),
        "{said:#?}"
    );
    assert!(
        said.iter()
            .any(|line| line.contains("is kept: once the merge is committed")),
        "{said:#?}"
    );
    assert!(
        !said
            .iter()
            .any(|line| line.contains("INTERRUPTED") || line.contains("STOPPED")),
        "{said:#?}"
    );
    assert_eq!(origin_main(world), tip, "a conflict pushes nothing");
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), judged);
    assert!(kept_record(world).is_some(), "{said:#?}");
    (judged, merged, tip, said)
}

/// Commit the merge a landing left conflicted in `world`'s work checkout, with each of `files`
/// holding its text: the merge commit.
fn resolve(world: &World, files: &[(&str, &str)]) -> String {
    for (file, text) in files {
        write(&world.work, file, text);
    }
    git(&world.work, &["add", "-A"]);
    git(&world.work, &["commit", "-q", "--no-edit"]);
    git(&world.work, &["rev-parse", "HEAD"])
}

/// A CONFLICTING MAIN KEEPS THE JUDGMENT AND STOPS ON THE CONFLICT: polled while the judgment
/// runs it interrupts nothing and is said once, the judgment finishes and is kept, the merge
/// stops on the conflict, left for the person, and nothing is pushed. NEGATIVE CONTROLS: the
/// same conflict where main's change reaches every crate the branch's change reaches, and one
/// in a path whose hand merge is certain to be judged in full (a file that replans every crate,
/// Markdown a file may read, a submodule's entry: none of them asks a reach), each interrupt
/// the run at the first poll, since nothing it would produce stands for the merge to come; the
/// landing stops on the conflict all the same, with no judgment kept.
#[test]
fn a_conflicting_main_keeps_the_judgment_and_stops_on_the_conflict() {
    let world = world();
    commit(&world.work, "shared.txt", &SHARED.replacen('4', "mine", 1));
    let (_, _, tip, said) = stopped_on_conflict_saying(&world, true, &|rival| {
        commit(rival, "shared.txt", &SHARED.replacen('4', "theirs", 1));
    });
    assert!(git(&world.work, &["status", "--porcelain"]).contains("UU shared.txt"));
    let went_on = format!(
        "verify land: origin/main moved to {tip}, which does not merge cleanly (the merge \
         conflicts in \"shared.txt\"): the judgment goes on and is kept, and the merge will \
         stop on the conflict for a person to resolve"
    );
    assert_eq!(
        said.iter().filter(|line| **line == went_on).count(),
        1,
        "{said:#?}"
    );

    let whole = |paths: Option<&[String]>| match paths {
        None => reaches(&["a", "b"]),
        Some(_) => reaches(&["a", "b", "c"]),
    };
    let unasked = |paths: Option<&[String]>| match paths {
        None => reaches(&["a", "b"]),
        Some(paths) => panic!("no reach is asked of {paths:?}"),
    };
    let named = "pub const NAMED: &str = \"docs/NAMED.md\";\n";
    let pin = |dir: &Path, object: &str| {
        // An empty directory is a submodule not checked out: the checkout stays clean.
        std::fs::create_dir_all(dir.join("vendor/x")).expect("the submodule's directory");
        git(
            dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{object},vendor/x"),
            ],
        );
        git(dir, &["commit", "-q", "-m", "move the pin"]);
    };
    let cases: [(&str, Reaching<'_>, &str); 4] = [
        (
            "shared.txt",
            &whole,
            "reaches every crate the branch's change reaches (a, b)",
        ),
        (
            "Cargo.lock",
            &unasked,
            "shares Cargo.lock, which can change how every crate is built or linted, so it is \
             judged in full",
        ),
        (
            "docs/NAMED.md",
            &unasked,
            "docs/NAMED.md is named in crates/b/src/lib.rs's code, so a build or test may read \
             it, so the merge that resolves it is judged in full",
        ),
        (
            "vendor/x",
            &unasked,
            "the merge that resolves it shares the submodule entry vendor/x, so it is judged in \
             full",
        ),
    ];
    for (file, reach, why) in cases {
        let world = match file {
            "docs/NAMED.md" => markdown_world(&[
                ("docs/NAMED.md", None, false),
                ("crates/b/src/lib.rs", Some(named), false),
            ]),
            _ => self::world(),
        };
        let side = |dir: &Path, text: &str| match file {
            "vendor/x" => pin(dir, &text.repeat(40)),
            "docs/NAMED.md" => drop(edit_line(dir, file, 4, text)),
            _ => drop(commit(dir, file, &SHARED.replacen('4', text, 1))),
        };
        let (mine, theirs) = match file {
            "vendor/x" => ("1", "2"),
            _ => ("mine", "theirs"),
        };
        side(&world.work, mine);
        let tip = RefCell::new(String::new());
        let (code, said, bases, _) = land_reaching(
            &world,
            crates(&["a"]),
            Lane::Pass,
            reach,
            |_, _, poll| {
                *tip.borrow_mut() = rival_does(&world, |rival| side(rival, theirs));
                assert!(poll(&mut || false), "{file}: nothing of the run stands");
                Ran::Interrupted { secs: 1 }
            },
            |base, _| panic!("{file}: no round (base {base})"),
        );
        assert_eq!(
            (code, bases.len()),
            (exit::CONFLICT, 1),
            "{file}: {said:#?}"
        );
        assert!(
            said.iter().any(|line| line.contains("INTERRUPTED")
                && line.contains(&format!("the merge conflicts in \"{file}\""))
                && line.ends_with(why)),
            "{file}: {said:#?}"
        );
        assert!(
            said.iter()
                .any(|line| line.contains("CONFLICT — merging") && line.contains("conflicts in")),
            "{file}: {said:#?}"
        );
        assert!(
            !said.iter().any(|line| line.contains("is kept")),
            "{file}: {said:#?}"
        );
        assert_eq!(kept_record(&world), None, "{file}");
        assert_eq!(origin_main(&world), tip.into_inner(), "{file}");
    }
}

/// THE MERGE A PERSON COMMITS AFTER A CONFLICT IS JUDGED OVER WHAT IT CHANGED FOR THE BRANCH:
/// its first parent is the kept judgment's commit and its second the main tip, so the next run
/// judges nothing in full. One round judges the merge since that tip over the paths it shares
/// ([`Git::shared`]), told that Git did not make it, and the landing is that merge, restated
/// with the kept judgment and the round. The shared paths hold every place the merge differs
/// from a merge of the two sides: the conflict as resolved; a change of main's the merge
/// dropped (a conflict resolved as the branch had it: the file is the judged commit's own, and
/// only [`Delta::mechanical`] says its dependents must be judged); a change of the branch's it
/// dropped; and an edit neither side made. NEGATIVE CONTROL: with the kept record gone the same
/// merge is judged in full.
#[test]
fn a_hand_merge_of_the_kept_judgment_is_judged_over_what_it_changed() {
    let mine = SHARED.replacen('4', "mine", 1);
    let both = SHARED.replacen('4', "both", 1);
    let cases: [Resolved<'_>; 4] = [
        (
            "the conflict resolved",
            vec![("shared.txt", &both)],
            &["shared.txt"],
        ),
        (
            "main's change dropped",
            vec![("shared.txt", &mine)],
            &["shared.txt"],
        ),
        (
            "more dropped and an edit neither side made",
            vec![
                ("shared.txt", &both),
                ("a/x.txt", "a\n"),
                ("b/y.txt", "b\n"),
                ("c/new.txt", "new\n"),
            ],
            &["a/x.txt", "b/y.txt", "c/new.txt", "shared.txt"],
        ),
        ("the record gone", vec![("shared.txt", &both)], &[]),
    ];
    for (case, resolution, shared) in cases {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        commit(&world.work, "shared.txt", &mine);
        let judged_tree = head_tree(&world);
        let (judged, merged, tip) = stopped_on_conflict(&world, true, &|rival| {
            commit(rival, "b/y.txt", "b2\n");
            commit(rival, "shared.txt", &SHARED.replacen('4', "theirs", 1));
        });
        let merge = resolve(&world, &resolution);
        let forgotten = case == "the record gone";
        if forgotten {
            git(&world.work, &["update-ref", "-d", KEPT]);
        }
        let deltas = RefCell::new(Vec::new());
        let (code, said, bases, rounds) = land_reaching(
            &world,
            crates(&["a"]),
            Lane::Pass,
            &narrow,
            |_, _, _| {
                assert!(
                    forgotten,
                    "{case}: a merge of a kept judgment is no judgment"
                );
                Ran::Ended(green())
            },
            |_, delta| {
                deltas.borrow_mut().push(delta.clone());
                round_green()
            },
        );
        assert_eq!(code, exit::PASS, "{case}: {said:#?}");
        let landing = origin_main(&world);
        let parents = git(&world.work, &["rev-parse", &format!("{landing}^@")]);
        assert_eq!(
            parents.lines().collect::<Vec<_>>(),
            [judged.as_str(), tip.as_str()],
            "{case}: the landing is the person's merge, restated"
        );
        let receipt = stated(&world, &landing).expect("stated");
        if forgotten {
            assert_eq!(bases, std::slice::from_ref(&tip), "{case}: {said:#?}");
            assert!(rounds.is_empty(), "{case}: {said:#?}");
            assert!(receipt.rechecked.is_empty(), "{case}");
            continue;
        }
        assert!(bases.is_empty(), "{case}: {said:#?}");
        assert_eq!(rounds, [(tip.clone(), merge.clone())], "{case}: {said:#?}");
        assert_eq!(
            deltas.into_inner(),
            [Delta {
                judged: judged.clone(),
                shared: shared.iter().map(|path| (*path).to_owned()).collect(),
                mechanical: false,
            }],
            "{case}"
        );
        assert!(
            said.iter().any(|line| line.contains(&format!(
                "HEAD merges {tip} into that commit: the merge is judged for what it changed"
            ))),
            "{case}: {said:#?}"
        );
        assert!(
            said.iter()
                .any(|line| line.contains("resolved or edited by hand")
                    && line.contains("no shared path's crate is judged alone")),
            "{case}: {said:#?}"
        );
        assert_eq!(
            (
                receipt.checked.len(),
                receipt.checked[0].tree.as_str(),
                receipt.checked[0].base.as_str()
            ),
            (1, judged_tree.as_str(), merged.as_str()),
            "{case}: the kept judgment, as it was made"
        );
        assert_eq!(
            receipt.rechecked,
            [Rechecked {
                tree: git(&world.work, &["rev-parse", &format!("{merge}^{{tree}}")]),
                rows: round_green()
                    .checks
                    .iter()
                    .map(|row| Row {
                        check: row.check.clone(),
                        word: row.word.clone(),
                    })
                    .collect(),
                base: tip.clone(),
                onto: tip.clone(),
                secs: round_green().secs,
                after: 1,
            }],
            "{case}"
        );
        assert_eq!((receipt.rejudges(), &receipt.composed_onto), (0, &None));
        assert_eq!(
            git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")]),
            git(&world.work, &["rev-parse", &format!("{merge}^{{tree}}")]),
            "{case}"
        );
        assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), landing, "{case}");
        assert_eq!(kept_record(&world), None, "{case}");
    }
}

/// A KEPT JUDGMENT STANDS ONLY FOR THE FIRST PARENT OF A MERGE OF MAIN INTO ITS OWN COMMIT.
/// NEGATIVE CONTROLS of [`Git::kept_merge`]: a merge whose first parent is the judged tree
/// under another commit, one whose second parent main does not hold, and a merge with a commit
/// on top of it are each judged in full, and nothing is judged by a round.
#[test]
fn a_merge_that_is_not_of_main_into_the_kept_commit_is_judged_in_full() {
    let both = SHARED.replacen('4', "both", 1);
    for case in [
        "another first parent",
        "a second parent off main",
        "a commit on top",
    ] {
        let world = world();
        commit(&world.work, "a/x.txt", "a2\n");
        commit(&world.work, "shared.txt", &SHARED.replacen('4', "mine", 1));
        let (_, _, tip) = stopped_on_conflict(&world, true, &|rival| {
            commit(rival, "shared.txt", &SHARED.replacen('4', "theirs", 1));
        });
        match case {
            "another first parent" => {
                git(&world.work, &["merge", "--abort"]);
                git(
                    &world.work,
                    &[
                        "commit",
                        "-q",
                        "--amend",
                        "-m",
                        "the judged commit, reworded",
                    ],
                );
                let mut merge = git_cmd(&world.work);
                merge.args(["merge", "-q", "--no-edit", &tip]);
                let out =
                    aterm_uds::deadline::output_within(&mut merge, aterm_spec::wait::MUST_HAPPEN)
                        .expect("git merge");
                assert!(!out.status.success(), "the merge conflicts again");
                resolve(&world, &[("shared.txt", &both)]);
            }
            "a second parent off main" => {
                git(&world.work, &["merge", "--abort"]);
                git(&world.work, &["checkout", "-q", "-b", "side", &tip]);
                let side = commit(&world.work, "c/z.txt", "c\n");
                git(&world.work, &["checkout", "-q", "feature"]);
                let mut merge = git_cmd(&world.work);
                merge.args(["merge", "-q", "--no-edit", &side]);
                let out =
                    aterm_uds::deadline::output_within(&mut merge, aterm_spec::wait::MUST_HAPPEN)
                        .expect("git merge");
                assert!(!out.status.success(), "the merge conflicts again");
                resolve(&world, &[("shared.txt", &both)]);
            }
            _ => {
                resolve(&world, &[("shared.txt", &both)]);
                commit(&world.work, "a/x.txt", "a3\n");
            }
        }
        let (code, said, bases, rounds) = land_reaching(
            &world,
            crates(&["a"]),
            Lane::Pass,
            &narrow,
            |_, _, _| Ran::Ended(green()),
            |base, _| panic!("{case}: judged in full, never by a round (base {base})"),
        );
        assert_eq!(code, exit::PASS, "{case}: {said:#?}");
        assert_eq!(bases, std::slice::from_ref(&tip), "{case}: {said:#?}");
        assert!(rounds.is_empty(), "{case}");
        assert!(
            said.iter()
                .any(|line| line.contains("the kept judgment is not resumed")),
            "{case}: {said:#?}"
        );
        let receipt = stated(&world, &origin_main(&world)).expect("stated");
        assert_eq!(
            (receipt.checked.len(), receipt.rechecked.len()),
            (1, 0),
            "{case}"
        );
        assert_eq!(receipt.checked[0].base, tip, "{case}");
    }
}

/// A HAND MERGE NO ROUND ANSWERS FOR IS JUDGED IN FULL, over the cone since the tip it merged,
/// the kept judgment and that judgment both stated. A lock both sides changed, resolved as the
/// branch had it, is the judged commit's own file: a round compares it with that commit and
/// finds no change, though main's manifests no longer agree with it. And Markdown a file may
/// read is in no cone, so a round's source readers alone would answer for a conflict in it.
/// NEGATIVE CONTROL: the same conflict in Markdown no code names is judged by a round, which
/// asks no reach.
#[test]
fn a_hand_merge_of_a_lock_or_of_markdown_a_file_reads_is_judged_in_full() {
    let named = "pub const NAMED: &str = \"docs/NAMED.md\";\n";
    let cases: [(&str, Vec<markdown::File<'_>>, Option<&str>); 3] = [
        (
            "Cargo.lock",
            vec![("Cargo.lock", None, false)],
            Some("was resolved or edited by hand and shares Cargo.lock"),
        ),
        (
            "docs/NAMED.md",
            vec![
                ("docs/NAMED.md", None, false),
                ("crates/b/src/lib.rs", Some(named), false),
            ],
            Some("docs/NAMED.md is named in crates/b/src/lib.rs's code"),
        ),
        (DOC, vec![], None),
    ];
    for (file, files, whole) in cases {
        let world = markdown_world(&files);
        commit(&world.work, "a/x.txt", "a2\n");
        edit_line(&world.work, file, 4, "mine");
        let judged_tree = head_tree(&world);
        // A conflict in a lock, or in Markdown a file may read, would cut a judgment that
        // polls it: this one has passed.
        let (judged, merged, tip) = stopped_on_conflict(&world, whole.is_none(), &|rival| {
            drop(edit_line(rival, file, 4, "theirs"));
        });
        // The lock as the branch had it; the Markdown with both edits.
        let ours = git(&world.work, &["show", &format!("{judged}:{file}")]);
        let resolved = if file == "Cargo.lock" {
            format!("{ours}\n")
        } else {
            SHARED.replacen('4', "both", 1)
        };
        let merge = resolve(&world, &[(file, &resolved)]);
        let (code, said, bases, rounds) = land_reaching(
            &world,
            crates(&["a"]),
            Lane::Pass,
            &|paths| match paths {
                None => reaches(&["a", "b"]),
                Some(paths) => panic!("{file}: no reach is asked ({paths:?})"),
            },
            |_, _, _| {
                assert!(whole.is_some(), "{file}: a round answers for it");
                Ran::Ended(green())
            },
            |_, delta| {
                assert!(whole.is_none(), "{file}: no round answers for it");
                assert_eq!(
                    (delta.shared.as_slice(), delta.mechanical),
                    (&[file.to_owned()][..], false),
                    "{file}"
                );
                rechecked()
            },
        );
        assert_eq!(code, exit::PASS, "{file}: {said:#?}");
        let landing = origin_main(&world);
        let parents = git(&world.work, &["rev-parse", &format!("{landing}^@")]);
        assert_eq!(
            parents.lines().collect::<Vec<_>>(),
            [judged.as_str(), tip.as_str()],
            "{file}"
        );
        let receipt = stated(&world, &landing).expect("stated");
        assert_eq!(
            (
                receipt.checked[0].tree.as_str(),
                receipt.checked[0].base.as_str()
            ),
            (judged_tree.as_str(), merged.as_str()),
            "{file}: the kept judgment is stated"
        );
        let Some(why) = whole else {
            assert!(bases.is_empty(), "{file}: {said:#?}");
            assert_eq!(rounds, [(tip.clone(), merge.clone())], "{file}");
            assert_eq!(
                (receipt.checked.len(), receipt.rechecked.len()),
                (1, 1),
                "{file}"
            );
            continue;
        };
        assert_eq!(bases, std::slice::from_ref(&tip), "{file}: {said:#?}");
        assert!(rounds.is_empty(), "{file}: {said:#?}");
        assert!(
            said.iter().any(|line| line.contains("no round answers for")
                && line.contains(why)
                && line.ends_with("it is judged in full")),
            "{file}: {said:#?}"
        );
        assert_eq!(
            (receipt.checked.len(), receipt.rechecked.len()),
            (2, 0),
            "{file}"
        );
        assert_eq!(
            (
                receipt.checked[1].tree.clone(),
                receipt.checked[1].base.as_str()
            ),
            (
                git(&world.work, &["rev-parse", &format!("{merge}^{{tree}}")]),
                tip.as_str()
            ),
            "{file}: the merge is judged over the cone since the tip it merged"
        );
        assert_eq!(kept_record(&world), None, "{file}");
    }
}

/// A hand merge that shares a submodule's entry is judged in full: here both sides moved the
/// pin and the merge keeps the branch's, so main's move is dropped. NEGATIVE CONTROL: the same
/// two sides and the same kept judgment, where the path is a file, are a round.
#[test]
fn a_hand_merge_that_shares_a_submodule_entry_is_judged_in_full() {
    for (mode, whole) in [("160000", true), ("100644", false)] {
        let world = world();
        let seed = origin_main(&world);
        let plumbing = Git {
            root: &world.work,
            make: &git_cmd,
        };
        // Each side's entry at `vendor/x`: two commits as pins, or two blobs as files.
        let objects = if whole {
            [
                commit(&world.work, "a/x.txt", "pin one\n"),
                commit(&world.work, "a/x.txt", "pin two\n"),
            ]
        } else {
            ["one", "two"].map(|text| {
                write(&world.work, "scratch.txt", text);
                let blob = git(&world.work, &["hash-object", "-w", "scratch.txt"]);
                std::fs::remove_file(world.work.join("scratch.txt")).expect("scratch");
                blob
            })
        };
        let seed_tree = git(&world.work, &["rev-parse", &format!("{seed}^{{tree}}")]);
        let with = |object: &str, parents: &[&str]| {
            git(&world.work, &["read-tree", &seed_tree]);
            git(
                &world.work,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{mode},{object},vendor/x"),
                ],
            );
            let tree = git(&world.work, &["write-tree"]);
            plumbing
                .commit(&tree, parents, "a side")
                .expect("commit-tree")
        };
        let judged = with(&objects[0], &[&seed]);
        let tip = with(&objects[1], &[&seed]);
        // The merge keeps the branch's entry.
        let merge = with(&objects[0], &[&judged, &tip]);
        let stated = Stated {
            checked: vec![Checked {
                tree: plumbing.tree(&judged).expect("tree"),
                rows: vec![],
                base: seed.clone(),
                secs: 1,
            }],
            ..Stated::default()
        };
        match round_delta(&plumbing, &merge, &tip, &stated).expect("git reads") {
            Rounded::Whole(why) => {
                assert!(whole, "{mode}: {why}");
                assert!(
                    why.contains("by hand and shares the submodule entry vendor/x"),
                    "{why}"
                );
            }
            Rounded::Round(delta) => {
                assert!(!whole, "{mode}: {delta:?}");
                assert_eq!(
                    delta,
                    Delta {
                        judged: judged.clone(),
                        shared: vec!["vendor/x".into()],
                        mechanical: false,
                    }
                );
            }
        }
    }
}

/// THE TOOL'S OWN MERGE IS JUDGED BY A ROUND AGAIN AFTER A ROUND OF IT WENT RED: the kept
/// judgment stands for its first parent, so the next run judges nothing in full; its round is
/// of the merge Git made ([`Delta::mechanical`]), and the landing states the judgment the
/// earlier run made beside it. NEGATIVE CONTROL: the first run's red round pushes nothing.
#[test]
fn a_round_that_went_red_is_run_again_from_the_kept_judgment() {
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let judged = git(&world.work, &["rev-parse", "HEAD"]);
    let tip = RefCell::new(String::new());
    let (code, said, bases, rounds) = land_reaching(
        &world,
        crates(&["a"]),
        Lane::Pass,
        &narrow,
        |_, _, _| {
            *tip.borrow_mut() =
                rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
            Ran::Ended(green())
        },
        |_, _| {
            ran(
                exit::FAILED,
                vec![
                    row("fmt", "ok"),
                    row("census", "ok"),
                    row("export-scan", "ok"),
                    red("test", &["-p a --lib -- mine"]),
                ],
                3,
            )
        },
    );
    let tip = tip.into_inner();
    assert_eq!(
        (code, bases.len(), rounds.len()),
        (exit::FAILED, 1, 1),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), tip, "a red round pushes nothing");
    let merge = git(&world.work, &["rev-parse", "HEAD"]);
    assert_eq!(rounds[0], (tip.clone(), merge.clone()));
    let judged_tree = git(&world.work, &["rev-parse", &format!("{judged}^{{tree}}")]);
    assert_eq!(
        said.last().map(String::as_str),
        Some(
            format!(
                "verify land: the judgment of {judged_tree}, the merge's first parent, is kept: \
                 amend the merge with the fix and run it again, and it is judged for what it \
                 changed for the branch; a commit on top of the merge is judged in full"
            )
            .as_str()
        ),
        "{said:#?}"
    );

    let (code, said, bases, rounds) = land_reaching(
        &world,
        crates(&["a"]),
        Lane::Pass,
        &narrow,
        |base, _, _| panic!("the kept judgment is not made again (base {base})"),
        |_, delta| {
            assert_eq!(
                *delta,
                Delta {
                    judged: judged.clone(),
                    shared: vec!["shared.txt".into()],
                    mechanical: true,
                }
            );
            round_green()
        },
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert!(bases.is_empty(), "{said:#?}");
    assert_eq!(rounds, [(tip.clone(), merge)], "{said:#?}");
    let landing = origin_main(&world);
    let receipt = stated(&world, &landing).expect("stated");
    assert_eq!(
        (receipt.checked.len(), receipt.rechecked.len()),
        (1, 1),
        "{said:#?}"
    );
    assert_eq!(receipt.rechecked[0].onto, tip);
    assert_eq!(kept_record(&world), None);
}

/// A WHOLE JUDGMENT OF THE MERGE THAT WENT RED IS RUN AGAIN IN FULL, NEVER AS A ROUND: a main
/// that moves after the judgment passed, where a round would judge every crate the branch's
/// change reaches, is merged and judged in full, and that judgment drops the record the first
/// one kept before it runs. It goes red; the next run finds HEAD a merge of main into the
/// commit the first judgment judged, with nothing kept for it, and judges it in full again.
/// With the record still kept that run would judge the merge by a round, which judges less
/// than the judgment that just failed, and could push what it found red.
#[test]
fn a_whole_judgment_of_the_merge_that_went_red_is_never_followed_by_a_round() {
    let whole = |paths: Option<&[String]>| match paths {
        None => reaches(&["a", "b"]),
        Some(_) => reaches(&["a", "b", "c"]),
    };
    let world = world();
    commit(&world.work, "a/x.txt", "a2\n");
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let tip = RefCell::new(String::new());
    let kept = RefCell::new(Vec::new());
    let (code, said, bases, rounds) = land_reaching(
        &world,
        crates(&["a"]),
        Lane::Pass,
        &whole,
        |_, n, _| {
            kept.borrow_mut().push(kept_record(&world).is_some());
            if n == 0 {
                *tip.borrow_mut() =
                    rival_moves(&world, "shared.txt", &SHARED.replacen('8', "eight", 1));
                return Ran::Ended(green());
            }
            Ran::Ended(ran(
                exit::FAILED,
                vec![red("test", &["-p b --lib -- theirs"])],
                3,
            ))
        },
        |base, _| panic!("a tip that reaches the branch's every crate has no round (base {base})"),
    );
    let tip = tip.into_inner();
    assert_eq!(
        (code, bases.len(), rounds.len()),
        (exit::FAILED, 2, 0),
        "{said:#?}"
    );
    assert_eq!(
        kept.into_inner(),
        [false, false],
        "the first judgment's record is dropped before the merge is judged in full"
    );
    assert_eq!(kept_record(&world), None, "{said:#?}");
    assert!(
        !said.iter().any(|line| line.contains("is kept")),
        "{said:#?}"
    );
    assert_eq!(origin_main(&world), tip, "a red judgment pushes nothing");
    let merge = git(&world.work, &["rev-parse", "HEAD"]);

    let (code, said, bases, rounds) = land_reaching(
        &world,
        crates(&["a"]),
        Lane::Pass,
        &whole,
        |_, _, _| Ran::Ended(green()),
        |base, _| panic!("the red judgment is made again in full, never as a round (base {base})"),
    );
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases, std::slice::from_ref(&tip), "{said:#?}");
    assert!(rounds.is_empty(), "{said:#?}");
    let receipt = stated(&world, &origin_main(&world)).expect("stated");
    assert_eq!(
        (receipt.checked.len(), receipt.rechecked.len()),
        (1, 0),
        "{said:#?}"
    );
    assert_eq!(
        receipt.checked[0].tree,
        git(&world.work, &["rev-parse", &format!("{merge}^{{tree}}")])
    );
}
