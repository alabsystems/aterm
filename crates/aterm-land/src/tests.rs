// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;
use std::collections::VecDeque;

fn exit(code: i32) -> Result<Outcome<usize>, String> {
    Ok(Outcome::Exit(code))
}

// A local test host's opaque value, never a Root, artifact or proof capability.
struct Qualified(Revision);
struct World {
    head: String,
    replies: VecDeque<Publication>,
    compositions: VecDeque<Composition>,
    runs: Vec<Revision>,
    sends: Vec<(String, usize)>,
    sessions: usize,
    merges: usize,
    rechecks: usize,
    interrupted: bool,
    refuse_at: Option<usize>,
    refuse_admission: bool,
    mismatch: bool,
    /// The revision `at` this image cannot qualify: the host yields it to another image.
    elsewhere_at: Option<String>,
    /// Whether this image cannot recheck: the host yields every recheck to another image.
    recheck_elsewhere: bool,
}
impl World {
    fn new() -> Self {
        Self {
            head: "checked".into(),
            replies: VecDeque::new(),
            compositions: VecDeque::new(),
            runs: vec![],
            sends: vec![],
            sessions: 0,
            merges: 0,
            rechecks: 0,
            interrupted: false,
            refuse_at: None,
            refuse_admission: false,
            mismatch: false,
            elsewhere_at: None,
            recheck_elsewhere: false,
        }
    }
    fn limits() -> Limits {
        Limits {
            rejudges: 3,
            composes: 10,
            stopped: 75,
            unjudged: 2,
        }
    }
    fn initial() -> Revision {
        Revision {
            at: "checked".into(),
            base: "main0".into(),
        }
    }
}
impl Host for World {
    type Qualification = Qualified;
    type PushSession = usize;
    fn qualify(&mut self, revision: &Revision) -> Result<Judgment<Qualified>, String> {
        if self.elsewhere_at.as_ref() == Some(&revision.at) {
            return Ok(Judgment::Elsewhere);
        }
        self.runs.push(revision.clone());
        if std::mem::take(&mut self.interrupted) {
            return Ok(Judgment::Interrupted("main1".into()));
        }
        Ok(Judgment::Passed(Qualified(revision.clone())))
    }
    fn recheck(&mut self, revision: &Revision) -> Result<Judgment<Qualified>, String> {
        if self.recheck_elsewhere {
            return Ok(Judgment::Elsewhere);
        }
        self.rechecks += 1;
        Ok(Judgment::Passed(Qualified(revision.clone())))
    }
    fn current(&self, at: &str) -> Result<Option<String>, String> {
        Ok((at != self.head).then(|| "checkout changed".into()))
    }
    fn merge(&mut self, tip: &str) -> Result<Merge, String> {
        self.merges += 1;
        self.head = format!("merge-{tip}");
        Ok(Merge::Clean)
    }
    fn head(&self) -> Result<String, String> {
        Ok(self.head.clone())
    }
    fn tree(&self, _: &str) -> Result<String, String> {
        Ok(if self.mismatch {
            "different"
        } else {
            "merged-tree"
        }
        .into())
    }
    fn composition(&self, _: &Revision, _: &str) -> Result<Composition, String> {
        unreachable!("scripted through interior-free adapter below")
    }
    fn begin_push(&mut self) -> Result<usize, String> {
        if self.refuse_admission {
            return Err("admitted publication owner unavailable".into());
        }
        self.sessions += 1;
        Ok(self.sessions)
    }
    fn publish(
        &mut self,
        request: PushRequest<'_, Qualified, usize>,
    ) -> Result<Publication, String> {
        let PushRequest {
            qualified: q,
            revision,
            onto,
            session,
            ..
        } = request;
        assert_eq!(
            &q.0, revision,
            "a different revision cannot replace the host's opaque value"
        );
        self.sends.push((onto.into(), *session));
        if self.refuse_at == Some(self.sends.len()) {
            return Err("actual current-input owner refused".into());
        }
        Ok(self.replies.pop_front().unwrap_or(Publication::Done(0)))
    }
    fn event(&mut self, _: Event<'_>) {}
}

// Composition is an observational read, independently of the mutable qualifier/publisher.
struct Script {
    world: World,
    decisions: std::cell::RefCell<VecDeque<Composition>>,
}
impl Host for Script {
    type Qualification = Qualified;
    type PushSession = usize;
    fn qualify(&mut self, r: &Revision) -> Result<Judgment<Qualified>, String> {
        self.world.qualify(r)
    }
    fn recheck(&mut self, r: &Revision) -> Result<Judgment<Qualified>, String> {
        self.world.recheck(r)
    }
    fn current(&self, at: &str) -> Result<Option<String>, String> {
        self.world.current(at)
    }
    fn merge(&mut self, tip: &str) -> Result<Merge, String> {
        self.world.merge(tip)
    }
    fn head(&self) -> Result<String, String> {
        self.world.head()
    }
    fn tree(&self, at: &str) -> Result<String, String> {
        self.world.tree(at)
    }
    fn composition(&self, _: &Revision, _: &str) -> Result<Composition, String> {
        self.decisions
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| "missing composition observation".into())
    }
    fn begin_push(&mut self) -> Result<usize, String> {
        self.world.begin_push()
    }
    fn publish(
        &mut self,
        request: PushRequest<'_, Qualified, usize>,
    ) -> Result<Publication, String> {
        self.world.publish(request)
    }
    fn event(&mut self, e: Event<'_>) {
        self.world.event(e);
    }
}
impl Script {
    fn new(mut world: World) -> Self {
        let decisions = std::mem::take(&mut world.compositions);
        Self {
            world,
            decisions: std::cell::RefCell::new(decisions),
        }
    }
}

#[test]
fn repeated_composition_keeps_the_actual_qualification_and_one_push_session() {
    let mut world = World::new();
    world.replies.extend([
        Publication::Moved("main1".into()),
        Publication::Moved("main2".into()),
    ]);
    for _ in 0..2 {
        world.compositions.push_back(Composition::Composes {
            tree: "merged-tree".into(),
            why: "host established correspondence".into(),
        });
    }
    let mut host = Script::new(world);
    assert_eq!(run(&mut host, World::initial(), World::limits()), exit(0));
    assert_eq!(host.world.runs.len(), 1);
    assert_eq!(host.world.merges, 0);
    assert_eq!(
        host.world.sends,
        [
            ("main0".into(), 1),
            ("main1".into(), 1),
            ("main2".into(), 1)
        ]
    );
}

#[test]
fn observational_composition_cannot_replace_actual_current_input_admission() {
    let mut world = World::new();
    world.refuse_at = Some(2);
    world.replies.push_back(Publication::Moved("main1".into()));
    world.compositions.push_back(Composition::Composes {
        tree: "merged-tree".into(),
        why: "disjoint paths".into(),
    });
    let mut host = Script::new(world);
    assert_eq!(
        run(&mut host, World::initial(), World::limits()),
        Err("actual current-input owner refused".into())
    );
    assert_eq!(host.world.sends.len(), 2);
    assert_eq!(host.world.runs.len(), 1);
}

#[test]
fn refused_admission_never_creates_a_session_or_calls_publication() {
    let mut world = World::new();
    world.refuse_admission = true;
    let mut host = Script::new(world);
    assert_eq!(
        run(&mut host, World::initial(), World::limits()),
        Err("admitted publication owner unavailable".into())
    );
    assert_eq!(host.world.sessions, 0);
    assert!(host.world.sends.is_empty());
    assert_eq!(host.world.runs.len(), 1);
}

#[test]
fn overlapping_or_interrupted_revision_requires_a_new_actual_qualification() {
    for interrupted in [false, true] {
        let mut world = World::new();
        world.interrupted = interrupted;
        if !interrupted {
            world.replies.push_back(Publication::Moved("main1".into()));
            world
                .compositions
                .push_back(Composition::Doomed("input changed".into()));
        }
        let mut host = Script::new(world);
        assert_eq!(run(&mut host, World::initial(), World::limits()), exit(0));
        assert_eq!(host.world.runs.len(), 2);
        assert_eq!(
            host.world.runs[1],
            Revision {
                at: "merge-main1".into(),
                base: "main1".into()
            }
        );
        assert_eq!(host.world.merges, 1);
    }
}

#[test]
fn recheck_applies_only_to_the_exact_proposed_merge_tree() {
    for mismatch in [false, true] {
        let mut world = World::new();
        world.mismatch = mismatch;
        world.replies.push_back(Publication::Moved("main1".into()));
        world.compositions.push_back(Composition::Rechecks {
            tree: "merged-tree".into(),
            shared: vec!["docs/input.md".into()],
        });
        let mut host = Script::new(world);
        assert_eq!(run(&mut host, World::initial(), World::limits()), exit(0));
        assert_eq!(host.world.rechecks, usize::from(!mismatch));
        assert_eq!(host.world.runs.len(), if mismatch { 2 } else { 1 });
    }
}

#[test]
fn partial_recheck_keeps_the_same_admitted_push_session_and_deadline() {
    let mut world = World::new();
    world.replies.extend([
        Publication::Moved("main1".into()),
        Publication::Moved("main2".into()),
    ]);
    world.compositions.extend([
        Composition::Rechecks {
            tree: "merged-tree".into(),
            shared: vec!["docs/input.md".into()],
        },
        Composition::Composes {
            tree: "merged-tree".into(),
            why: "host correspondence".into(),
        },
    ]);
    let mut host = Script::new(world);
    assert_eq!(run(&mut host, World::initial(), World::limits()), exit(0));
    assert_eq!(host.world.rechecks, 1);
    assert_eq!(host.world.runs.len(), 1);
    assert_eq!(
        host.world.sessions, 1,
        "a partial recheck cannot mint a new admission/deadline"
    );
    assert_eq!(
        host.world.sends,
        [
            ("main0".into(), 1),
            ("main1".into(), 1),
            ("main2".into(), 1)
        ]
    );
}

#[test]
fn exhaustion_stops_without_another_publication() {
    let mut world = World::new();
    world.interrupted = true;
    let mut host = Script::new(world);
    let mut limits = World::limits();
    limits.rejudges = 0;
    assert_eq!(run(&mut host, World::initial(), limits), exit(75));
    assert!(host.world.sends.is_empty());
    assert_eq!(host.world.merges, 0);
}

#[test]
fn only_a_known_presend_obstruction_retries_under_one_shared_budget() {
    let attempts = Cell::new(0);
    let waits = Cell::new(0);
    let mut count = 0;
    let result = push_after_intents_with_count(
        2,
        &mut count,
        &|| {
            attempts.set(attempts.get() + 1);
            Ok(Attempt::Blocked {
                intent: attempts.get(),
                why: "held".into(),
            })
        },
        &|_| {
            waits.set(waits.get() + 1);
            Ok(())
        },
        &|| Ok(Observation::Unchanged),
        &|_| panic!("an obstruction is not a rejected write"),
    );
    assert!(result.is_err());
    assert_eq!((count, attempts.get(), waits.get()), (2, 3, 2));
    for unknown in [false, true] {
        attempts.set(0);
        let mut count = 0;
        let result = push_after_intents_with_count::<usize>(
            2,
            &mut count,
            &|| {
                attempts.set(attempts.get() + 1);
                if unknown {
                    Err("unknown write".into())
                } else {
                    Ok(Attempt::Rejected("refused".into()))
                }
            },
            &|_| panic!("no wait"),
            &|| panic!("no observation"),
            &|why| Err(why.into()),
        );
        assert!(result.is_err());
        assert_eq!((count, attempts.get()), (0, 1));
    }
}

#[test]
fn only_a_new_whole_qualification_starts_a_new_composition_epoch() {
    let mut world = World::new();
    world.replies.extend([
        Publication::Moved("main1".into()),
        Publication::Moved("main2".into()),
        Publication::Moved("main3".into()),
    ]);
    world.compositions.extend([
        Composition::Composes {
            tree: "merged-tree".into(),
            why: "first epoch".into(),
        },
        Composition::Doomed("new input".into()),
        Composition::Composes {
            tree: "merged-tree".into(),
            why: "second epoch".into(),
        },
    ]);
    let mut host = Script::new(world);
    let mut limits = World::limits();
    limits.composes = 1;
    assert_eq!(run(&mut host, World::initial(), limits), exit(0));
    assert_eq!(host.world.runs.len(), 2);
    assert_eq!(host.world.sessions, 2);
    assert_eq!(
        host.world.sends,
        [
            ("main0".into(), 1),
            ("main1".into(), 1),
            ("main2".into(), 2),
            ("main3".into(), 2)
        ]
    );
}

#[test]
fn polling_keeps_composable_tips_and_cuts_only_a_new_incompatible_tip() {
    let mut watch = Watch::new("main0");
    let composed = Cell::new(0);
    let mut moot = || false;
    let mut policy = |_: &str| {
        composed.set(composed.get() + 1);
        Ok(Composition::Composes {
            tree: "tree".into(),
            why: "host correspondence".into(),
        })
    };
    for _ in 0..2 {
        assert!(!watch.poll(
            &mut moot,
            &mut |_| Ok("main1".into()),
            &mut policy,
            &mut |_| panic!("unexpected failed read")
        ));
    }
    assert_eq!(
        composed.get(),
        1,
        "the same observed composable tip needs no repeated composition read"
    );
    assert!(watch.poll(
        &mut moot,
        &mut |_| Ok("main2".into()),
        &mut |_| Ok(Composition::Doomed("changed input".into())),
        &mut |_| panic!("no failure")
    ));
    assert!(watch.poll(
        &mut moot,
        &mut |_| panic!("no read after a cut"),
        &mut |_| panic!("no composition"),
        &mut |_| panic!("no failure")
    ));
    assert_eq!(
        watch.into_doomed(),
        Some(("main2".into(), "changed input".into()))
    );
}

#[test]
fn failed_or_moot_remote_reads_never_supply_a_tip_or_repeated_diagnostic() {
    let mut watch = Watch::new("main0");
    let notes = std::cell::RefCell::new(Vec::new());
    for _ in 0..2 {
        assert!(!watch.poll(
            &mut || false,
            &mut |_| Err("unavailable".into()),
            &mut |_| panic!("no observed tip"),
            &mut |why| notes.borrow_mut().push(why.to_owned())
        ));
    }
    assert_eq!(*notes.borrow(), ["unavailable"]);
    assert!(!watch.poll(
        &mut || true,
        &mut |_| Err("ended".into()),
        &mut |_| panic!("no tip"),
        &mut |_| panic!("moot read is silent")
    ));
    assert!(!watch.poll(
        &mut || false,
        &mut |_| Ok("main0".into()),
        &mut |_| panic!("same main needs no composition"),
        &mut |_| panic!("healthy read")
    ));
    assert!(!watch.poll(
        &mut || false,
        &mut |_| Err("unavailable".into()),
        &mut |_| panic!("no tip"),
        &mut |why| notes.borrow_mut().push(why.to_owned())
    ));
    assert_eq!(*notes.borrow(), ["unavailable", "unavailable"]);
    assert!(watch.into_doomed().is_none());
}

/// A qualification the host cannot run in this image yields before it runs: the loop ends
/// with that step's checkpoint (its revision, the re-judges spent and no composition count)
/// and no session, having begun no push. NEGATIVE CONTROL: the same world qualifying here
/// merges, qualifies again and publishes.
#[test]
fn a_host_that_cannot_qualify_here_yields_before_the_qualification() {
    for yields in [true, false] {
        let mut world = World::new();
        world.interrupted = true;
        if yields {
            world.elsewhere_at = Some("merge-main1".into());
        }
        let mut host = Script::new(world);
        let outcome = run(&mut host, World::initial(), World::limits());
        if yields {
            assert_eq!(
                outcome,
                Ok(Outcome::Elsewhere {
                    checkpoint: Checkpoint {
                        step: Step::Qualify,
                        revision: Revision {
                            at: "merge-main1".into(),
                            base: "main1".into(),
                        },
                        rejudges: 1,
                        composes: 0,
                    },
                    session: None,
                })
            );
            assert_eq!(host.world.runs.len(), 1);
            assert_eq!(host.world.sessions, 0);
            assert!(host.world.sends.is_empty());
        } else {
            assert_eq!(outcome, exit(0));
            assert_eq!(host.world.runs.len(), 2);
            assert_eq!(host.world.sends.len(), 1);
        }
        assert_eq!(host.world.merges, 1);
    }
}

/// A resumed qualification keeps the re-judges spent before it: at the bound, the next
/// interruption stops the landing with nothing merged. NEGATIVE CONTROL: one re-judge fewer
/// merges the tip and qualifies again.
#[test]
fn a_resumed_qualify_keeps_the_rejudge_count() {
    for (rejudges, want, merges) in [(3, 75, 0), (2, 0, 1)] {
        let mut world = World::new();
        world.interrupted = true;
        let mut host = Script::new(world);
        let checkpoint = Checkpoint {
            step: Step::Qualify,
            revision: World::initial(),
            rejudges,
            composes: 0,
        };
        assert_eq!(
            resume(&mut host, checkpoint, None, World::limits()),
            exit(want),
            "rejudges {rejudges}"
        );
        assert_eq!(host.world.merges, merges, "rejudges {rejudges}");
        assert_eq!(host.world.runs.len(), 1 + merges, "rejudges {rejudges}");
    }
}

/// A recheck the host cannot run here yields with the compositions spent since the last whole
/// qualification and the host's own push session. Resumed in another world, the recheck runs
/// once, publication reuses that session (no new one begins) and the composition bound goes on
/// from the carried count: exactly the remaining races compose before the landing stops.
#[test]
fn a_recheck_checkpoint_carries_its_composition_count_and_the_hosts_session() {
    let composed = 2;
    let mut world = World::new();
    world.recheck_elsewhere = true;
    for tip in 1..=composed + 1 {
        world
            .replies
            .push_back(Publication::Moved(format!("main{tip}")));
    }
    for _ in 0..composed {
        world.compositions.push_back(Composition::Composes {
            tree: "merged-tree".into(),
            why: "disjoint".into(),
        });
    }
    world.compositions.push_back(Composition::Rechecks {
        tree: "merged-tree".into(),
        shared: vec!["docs/input.md".into()],
    });
    let mut host = Script::new(world);
    let tip = format!("main{}", composed + 1);
    let checkpoint = Checkpoint {
        step: Step::Recheck,
        revision: Revision {
            at: format!("merge-{tip}"),
            base: tip.clone(),
        },
        rejudges: 0,
        composes: composed + 1,
    };
    assert_eq!(
        run(&mut host, World::initial(), World::limits()),
        Ok(Outcome::Elsewhere {
            checkpoint: checkpoint.clone(),
            session: Some(1),
        })
    );
    assert_eq!(host.world.rechecks, 0);

    let limits = World::limits();
    let remaining = limits.composes - (composed + 1);
    let mut resumed = World::new();
    resumed.head = format!("merge-{tip}");
    for race in 0..=remaining {
        resumed
            .replies
            .push_back(Publication::Moved(format!("later{race}")));
        resumed.compositions.push_back(Composition::Composes {
            tree: "merged-tree".into(),
            why: "disjoint".into(),
        });
    }
    let mut host = Script::new(resumed);
    assert_eq!(resume(&mut host, checkpoint, Some(1), limits), exit(75));
    assert_eq!(host.world.rechecks, 1);
    assert_eq!(host.world.sessions, 0, "the carried session is reused");
    assert_eq!(host.world.sends.len(), remaining + 1);
    assert!(host.world.sends.iter().all(|(_, session)| *session == 1));
    assert_eq!(host.world.sends[0].0, tip);
}

/// A checkpoint is refused, before any step runs, past this image's re-judge or composition
/// bound, with a session a qualification never carries or without the one a recheck needs,
/// or with a composition count before a qualification: [`admit`] names the same refusal, so a
/// host can tell it from what a resumed step does.
#[test]
fn a_checkpoint_past_this_images_bounds_or_missing_its_session_is_refused() {
    let point = |step, rejudges, composes| Checkpoint {
        step,
        revision: World::initial(),
        rejudges,
        composes,
    };
    for (checkpoint, session, why) in [
        (
            point(Step::Qualify, 4, 0),
            None,
            "the landing was handed over at re-judge 4, past this image's bound of 3",
        ),
        (
            point(Step::Recheck, 0, 11),
            Some(1),
            "landing race count exceeds its original bound",
        ),
        (
            point(Step::Qualify, 0, 0),
            Some(1),
            "a qualify checkpoint carries no push session",
        ),
        (
            point(Step::Qualify, 0, 1),
            None,
            "a qualify checkpoint carries no composition count",
        ),
        (
            point(Step::Recheck, 0, 1),
            None,
            "a recheck checkpoint carries no push session",
        ),
    ] {
        assert_eq!(
            admit(&checkpoint, session.is_some(), &World::limits()),
            Err(why.into())
        );
        let mut host = Script::new(World::new());
        assert_eq!(
            resume(&mut host, checkpoint, session, World::limits()),
            Err(why.into())
        );
        assert!(host.world.runs.is_empty() && host.world.rechecks == 0);
    }
}

/// A resumed step reads the checkout first: one that left the checkpoint's revision ends as
/// unjudged and runs nothing. NEGATIVE CONTROL: at that revision the same step runs.
#[test]
fn resume_reads_the_checkout_before_its_step() {
    for (step, session) in [(Step::Qualify, None), (Step::Recheck, Some(1))] {
        for moved in [true, false] {
            let mut world = World::new();
            if moved {
                world.head = "elsewhere".into();
            }
            let mut host = Script::new(world);
            let checkpoint = Checkpoint {
                step,
                revision: World::initial(),
                rejudges: 0,
                composes: 0,
            };
            assert_eq!(
                admit(&checkpoint, session.is_some(), &World::limits()),
                Ok(())
            );
            let outcome = resume(&mut host, checkpoint, session, World::limits());
            let ran = host.world.runs.len() + host.world.rechecks;
            if moved {
                assert_eq!(outcome, exit(World::limits().unjudged), "{step:?}");
                assert_eq!(ran, 0, "{step:?}");
            } else {
                assert_eq!(outcome, exit(0), "{step:?}");
                assert_eq!(ran, 1, "{step:?}");
            }
        }
    }
}
