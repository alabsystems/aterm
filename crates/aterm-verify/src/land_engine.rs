// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Aterm's policy and actual transports, adapted to the one shared landing loop, and the
//! landing's handoff where a step's own verifier is another image ([`finish`]).
use super::*;
use aterm_land::{Checkpoint, Event, Host, Judgment, Outcome, Publication, PushRequest, Revision};

// This is only Aterm's engineering judgment. It is neither a proof nor Trust's source
// qualification owner; no saved event or report constructs it, with one exception: this tool's
// own kept record of a judgment it passed ([`Git::keep`]), resumed only where HEAD is still the
// judged commit and holds the judged tree ([`Git::kept`]), stating that judgment as it was made.
// The record is a ref of the worktree; whoever can write it can push by hand as well, so it
// grants nothing the push itself did not already need.
struct Qualified {
    at: String,
    base: String,
}

pub(super) fn composition(value: Composition) -> aterm_land::Composition {
    match value {
        Composition::Composes { tree, why } => aterm_land::Composition::Composes { tree, why },
        Composition::Rechecks { tree, shared } => {
            aterm_land::Composition::Rechecks { tree, shared }
        }
        Composition::Doomed(why) => aterm_land::Composition::Doomed(why),
    }
}
impl Qualified {
    fn new(revision: &Revision) -> Self {
        Self {
            at: revision.at.clone(),
            base: revision.base.clone(),
        }
    }
}

struct Adapter<'a, 'g, 's> {
    git: &'a Git<'g>,
    seams: &'a mut Seams<'s>,
    subject: &'a str,
    words: &'a [String],
    held: Held<'a>,
    /// The image a step yielded to ([`Judging::Elsewhere`], [`Rechecking::Elsewhere`]).
    next: Option<Image>,
    /// A judgment an earlier run kept for this HEAD: the first step (a qualification, or a
    /// handed recheck) is this one, where it is of that judgment's own revision.
    resumed: Option<Kept>,
    /// The tree whose passed judgment is kept for a later run.
    kept: Option<String>,
    /// Main's tip, where a resumed landing found it moved from the main commit its kept
    /// judgment merged: the first publication composes onto it before any write.
    moved: Option<String>,
    /// What a later run must settle before it pushes again ([`Wire::unsettled`]).
    unsettled: Option<String>,
}

fn limits() -> aterm_land::Limits {
    aterm_land::Limits {
        rejudges: REJUDGES,
        composes: COMPOSES,
        stopped: STOPPED,
        unjudged: exit::COULD_NOT_RUN,
    }
}

/// Land `revision` (the checkout's HEAD over the main commit it merged), as `own`, from its
/// first qualification: a judgment, or `resumed`, the one an earlier run kept for it, with
/// main's tip where it `moved` since.
pub(super) fn drive(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    own: &Own<'_>,
    subject: &str,
    revision: Revision,
    resumed: Option<Kept>,
    moved: Option<String>,
) -> Result<i32, String> {
    let mut adapter = Adapter {
        git,
        seams,
        subject,
        words: own.words,
        held: Held {
            branch: None,
            cone: revision.base.clone(),
            stated: Stated::default(),
            own: own.image,
            hop: 0,
        },
        next: None,
        resumed,
        kept: None,
        moved,
        unsettled: None,
    };
    let ran = aterm_land::run(&mut adapter, revision, limits());
    conclude(adapter, ran)
}

/// Resume the step `handoff` stopped before, as `own`, with the re-judges and compositions it
/// spent, the branch's crates or the cone they are read over, and its receipt; that step hands
/// on while it has taken fewer than [`HOPS`] handoffs ([`built`]). A checkpoint past this
/// build's bounds ([`aterm_land::admit`]) could not run, naming the rerun. A recheck resumes in
/// its judgment's push, whose session holds nothing ([`Wire`]): no deadline crosses the hop.
///
/// Where the judgment kept for this checkout ([`Git::kept`]) is of the step's own revision
/// (its commit, holding the kept tree, over the main commit the kept judgment merged) and the
/// checkout still stands at that commit, the step is not run again: it is that judgment, with
/// its receipt, as the next run would resume it ([`drive`]). Any other record leaves the step
/// to run (it judges another revision), and a checkout that left the commit is refused by the
/// step's own check of it.
pub(super) fn resume(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    own: &Own<'_>,
    subject: &str,
    handoff: Handoff,
) -> Result<i32, String> {
    let Handoff {
        step,
        hop,
        at,
        base,
        rejudges,
        composes,
        cone,
        branch,
        stated,
        ..
    } = handoff;
    let checkpoint = Checkpoint {
        step,
        revision: Revision { at, base },
        rejudges,
        composes,
    };
    let session = (step == aterm_land::Step::Recheck).then_some(());
    aterm_land::admit(&checkpoint, session.is_some(), &limits())
        .map_err(|why| format!("{why}; {RERUN}"))?;
    let Revision { at, base } = &checkpoint.revision;
    let standing = || git.moved_from(at).is_ok_and(|moved| moved.is_none());
    let resumed = match git.kept(at) {
        Ok(Some(Ok(kept))) if kept.merged == *base && standing() => {
            (seams.say)(&format!(
                "verify land: resuming at the push: an earlier run judged {} over the cone since \
                 {}, and HEAD is still that commit",
                kept.tree, kept.merged
            ));
            Some(kept)
        }
        Ok(_) | Err(_) => None,
    };
    let mut adapter = Adapter {
        git,
        seams,
        subject,
        words: own.words,
        held: Held {
            branch,
            cone,
            stated,
            own: own.image,
            hop,
        },
        next: None,
        resumed,
        kept: None,
        moved: None,
        unsettled: None,
    };
    let ran = aterm_land::resume(&mut adapter, checkpoint, session, limits());
    conclude(adapter, ran)
}

/// How the landing that ran as `adapter` ends: where a step yielded to another image, its
/// handoff ([`finish`]); else its exit or error, which, where a judgment that passed is kept
/// and was not landed, says so, and what must be settled before a run pushes again.
fn conclude(
    mut adapter: Adapter<'_, '_, '_>,
    ran: Result<Outcome<()>, String>,
) -> Result<i32, String> {
    let ran = match ran {
        Ok(Outcome::Elsewhere { checkpoint, .. }) => return finish(adapter, checkpoint),
        Ok(Outcome::Exit(code)) => Ok(code),
        Err(why) => Err(why),
    };
    let unsettled = adapter.unsettled.take();
    let resume = |tree: &String| match &unsettled {
        None => format!(
            "the judgment of {tree} is kept: run it again and, while HEAD is the judged commit, \
             it resumes at the push"
        ),
        Some(unsettled) => format!(
            "the judgment of {tree} is kept, but {unsettled}: settle that before running it \
             again, which, while HEAD is the judged commit, resumes at the push"
        ),
    };
    match (ran, &adapter.kept) {
        (Err(why), Some(tree)) => Err(format!("{why}; {}", resume(tree))),
        (Ok(code), Some(tree)) if code != exit::PASS => {
            (adapter.seams.say)(&format!("verify land: {}", resume(tree)));
            Ok(code)
        }
        (ran, _) => ran,
    }
}

/// The handoff of the step `checkpoint` stopped before to the image it yielded to: the
/// checkpoint, the step's next hop, the receipt, the branch's crates or their cone and the
/// person's words, written in the checkout's git directory and handed over
/// ([`Seams::hand_over`]). The file is removed once that returns. A step yields only before it
/// runs, where the engine has merged a new main (a re-judge, a recheck) or the landing is at
/// its first judgment: no judgment is kept for that HEAD, and a write whose outcome is unknown
/// ended the landing before it, so the handoff leaves nothing to say.
fn finish(adapter: Adapter<'_, '_, '_>, checkpoint: Checkpoint) -> Result<i32, String> {
    let Adapter {
        git,
        seams,
        words,
        held,
        next,
        ..
    } = adapter;
    let image = next.ok_or("the landing yielded with no image to hand to")?;
    let handoff = Handoff {
        root: git.root.to_owned(),
        image: image.digest.clone(),
        words: words.to_vec(),
        step: checkpoint.step,
        hop: held.hop + 1,
        at: checkpoint.revision.at,
        base: checkpoint.revision.base,
        rejudges: checkpoint.rejudges,
        composes: checkpoint.composes,
        cone: held.cone,
        branch: held.branch,
        stated: held.stated,
    };
    let file = git.handoff_file()?;
    handoff::write(&file, &handoff)?;
    let tree = git.tree(&handoff.at)?;
    (seams.say)(&format!(
        "verify land: {tree}'s verifier is not this landing's image ({}): the landing goes on as \
         {} ({}) at the {} of {} over {}",
        short(held.own),
        image.path.display(),
        short(&image.digest),
        step_noun(handoff.step),
        handoff.at,
        handoff.base
    ));
    let code = (seams.hand_over)(&image, &file, git.root);
    // The receiver took it, or the hand-over failed and nothing will.
    let _ = std::fs::remove_file(&file);
    Ok(code)
}

impl Adapter<'_, '_, '_> {
    /// Keep the judgment of `revision` that just passed ([`Git::keep`]), before anything is
    /// pushed: no ending of the push loses it.
    fn keep(&mut self, revision: &Revision) {
        self.kept = match self
            .git
            .keep(&revision.at, &revision.base, &self.held.stated)
        {
            Ok(tree) => Some(tree),
            Err(why) => {
                (self.seams.say)(&format!(
                    "verify land: the judgment was not kept for a later run ({why}); the \
                     landing goes on"
                ));
                None
            }
        };
    }

    /// The judgment an earlier run kept (the `resumed` field) as the outcome of the landing's
    /// first step, which it stands for only where that step is of the revision it was made on:
    /// HEAD, holding the kept tree, over the main commit it merged. It ran nowhere here, so the
    /// next step starts at hop 0.
    fn take_resumed(&mut self, revision: &Revision) -> Result<Option<Judgment<Qualified>>, String> {
        let Some(kept) = self.resumed.take() else {
            return Ok(None);
        };
        if self.git.head()? == revision.at
            && self.git.tree(&revision.at)? == kept.tree
            && revision.base == kept.merged
        {
            self.held.stated = kept.stated;
            self.held.hop = 0;
            self.kept = Some(kept.tree);
            return Ok(Some(Judgment::Passed(Qualified::new(revision))));
        }
        Err("the kept judgment is not of the revision the landing began with".into())
    }
}

impl Host for Adapter<'_, '_, '_> {
    type Qualification = Qualified;
    /// Nothing: no deadline spans the landings of one judgment ([`Wire`]), so none crosses a
    /// handoff either.
    type PushSession = ();
    fn qualify(&mut self, revision: &Revision) -> Result<Judgment<Qualified>, String> {
        if let Some(kept) = self.take_resumed(revision)? {
            return Ok(kept);
        }
        Ok(
            match super::judge(
                self.git,
                self.seams,
                &revision.at,
                &revision.base,
                &mut self.held,
            )? {
                Judging::Passed => {
                    self.keep(revision);
                    Judgment::Passed(Qualified::new(revision))
                }
                Judging::Exit(code) => Judgment::Exit(code),
                Judging::Interrupted(tip) => Judgment::Interrupted(tip),
                Judging::Elsewhere(image) => {
                    self.next = Some(image);
                    Judgment::Elsewhere
                }
            },
        )
    }
    fn recheck(&mut self, revision: &Revision) -> Result<Judgment<Qualified>, String> {
        if let Some(kept) = self.take_resumed(revision)? {
            return Ok(kept);
        }
        Ok(
            match super::recheck(
                self.git,
                self.seams,
                &revision.at,
                &revision.base,
                &mut self.held,
            )? {
                Rechecking::Passed => {
                    self.keep(revision);
                    Judgment::Passed(Qualified::new(revision))
                }
                Rechecking::Exit(code) => Judgment::Exit(code),
                Rechecking::Elsewhere(image) => {
                    self.next = Some(image);
                    Judgment::Elsewhere
                }
            },
        )
    }
    fn current(&self, at: &str) -> Result<Option<String>, String> {
        self.git.moved_from(at)
    }
    fn merge(&mut self, tip: &str) -> Result<aterm_land::Merge, String> {
        // HEAD moves past the judged commit: the kept judgment no longer resumes this checkout.
        self.kept = None;
        Ok(match self.git.merge(tip, self.subject)? {
            Merge::Clean => aterm_land::Merge::Clean,
            Merge::Conflict(paths) => aterm_land::Merge::Exit(conflicted(self.seams, tip, &paths)),
        })
    }
    fn head(&self) -> Result<String, String> {
        self.git.head()
    }
    fn tree(&self, at: &str) -> Result<String, String> {
        self.git.tree(at)
    }
    fn composition(
        &self,
        revision: &Revision,
        tip: &str,
    ) -> Result<aterm_land::Composition, String> {
        Ok(composition(super::composition(
            self.git,
            &revision.base,
            tip,
            &revision.at,
        )?))
    }
    fn begin_push(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn publish(&mut self, request: PushRequest<'_, Qualified, ()>) -> Result<Publication, String> {
        let PushRequest {
            qualified,
            revision,
            onto,
            composed_tree,
            session: (),
            budget,
        } = request;
        if qualified.at != revision.at || qualified.base != revision.base {
            return Err("landing replaced its actual qualified revision".into());
        }
        // A resumed landing already read main's new tip: it is composed onto before any write,
        // which onto the kept judgment's own main would be refused.
        if let Some(tip) = self.moved.take()
            && onto == revision.base
            && composed_tree.is_none()
        {
            return Ok(Publication::Moved(tip));
        }
        self.held.stated.composed_onto = (onto != revision.base).then(|| onto.to_owned());
        let message = format!("{}\n\n{}", self.subject, self.held.stated.trailers());
        let parents = self.git.parents(&revision.at)?;
        let landing = match composed_tree {
            Some(tree) => self.git.commit(tree, &[&revision.at, onto], &message)?,
            None if parents.len() == 2 && parents[1] == onto => {
                let own = self
                    .git
                    .local(&["log", "-1", "--format=%B", &revision.at])?;
                let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
                self.git.commit(
                    &self.git.tree(&revision.at)?,
                    &parents,
                    &format!("{own}\n\n{}", self.held.stated.trailers()),
                )?
            }
            None => self.git.commit(
                &self.git.tree(&revision.at)?,
                &[&revision.at, onto],
                &message,
            )?,
        };
        let pushed = {
            let say = |line: &str| (self.seams.say)(line);
            let say = std::cell::RefCell::new(say);
            let say = |line: &str| (say.borrow_mut())(line);
            let wire = Wire::new(self.git, self.seams.remote, self.seams.pause, &say);
            let pushed = wire.push(&landing, onto, budget);
            let unsettled = wire.unsettled.take();
            drop(wire);
            if pushed.is_err() {
                self.unsettled = unsettled;
            }
            pushed?
        };
        match pushed {
            Some(tip) => Ok(Publication::Moved(tip)),
            None => {
                self.git.forget();
                self.kept = None;
                let adopted = self.git.adopt(&landing, &revision.at);
                (self.seams.say)(&landed_line(&landing, &self.held.stated));
                if let Err(why) = adopted {
                    (self.seams.say)(&format!(
                        "verify land: the landing is on {REMOTE}/{MAIN}, but {why}"
                    ));
                }
                Ok(Publication::Done(exit::PASS))
            }
        }
    }
    fn event(&mut self, event: Event<'_>) {
        let line = match event {
            // Only a resumed step reads the checkout while it holds a hop: before it runs.
            Event::CheckoutMoved(why) if self.held.hop > 0 => {
                format!("verify land: COULD NOT RUN — {why}; {RERUN_MOVED}")
            }
            Event::CheckoutMoved(why) => {
                format!("verify land: COULD NOT RUN — {why}; nothing was pushed")
            }
            Event::MainMoved(tip) => {
                format!("verify land: {REMOTE}/{MAIN} moved to {tip} before the push")
            }
            Event::Composing { tip, why } => format!("verify land: composing onto {tip}: {why}"),
            Event::RecheckNeeded { tip, shared } => format!(
                "verify land: {tip} composes once {} pass on the merge: the paths it shares with the branch are only Markdown no build, include or test reads ({})",
                RECHECKS.join(", "),
                shared.join(", ")
            ),
            Event::DoesNotCompose { tip, why } => {
                format!("verify land: {tip} does not compose: {why}")
            }
            Event::MergeDiffers {
                tip,
                made,
                expected,
            } => format!(
                "verify land: merging {tip} made {made}, not {expected}, the tree the Markdown rule tested: it is judged in full"
            ),
            Event::StoppedComposes { base, limit } => format!(
                "verify land: STOPPED after {limit} push/composition races; the branch is merged with {base} and judged, and nothing was pushed. Run it again"
            ),
            Event::StoppedRejudges {
                base,
                limit,
                interrupted,
            } => {
                let judged = if interrupted {
                    "its judgment there interrupted"
                } else {
                    "judged"
                };
                format!(
                    "verify land: STOPPED after {limit} re-judges — the branch is merged with {base}, {judged}, and nothing was pushed. Run it again"
                )
            }
            Event::StoppedUnjudged { tip, limit } => format!(
                "verify land: STOPPED after {limit} re-judges — the branch is merged with {tip}, not judged there, and nothing was pushed. Run it again"
            ),
        };
        (self.seams.say)(&line);
    }
}
