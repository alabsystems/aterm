// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One landing state machine, without a process, credential or qualification constructor.
//! A host retains its actual source, check, resource and remote owners. Its associated
//! qualification and push-session types cannot be reconstructed by this crate from a
//! report, path, commit, successful status or serialized data. Composition policy belongs
//! to that host: disjoint paths alone grant no check reuse or publication authority.
//!
//! A host may stop before a qualification or a source recheck and continue in another image
//! of itself ([`Judgment::Elsewhere`], [`Outcome::Elsewhere`], [`resume`]). The loop then
//! yields a [`Checkpoint`]: DATA (the step, its revision and two counts) that names no
//! qualification. Only the host carries its own push session across; this crate never
//! constructs a session or a qualification.

/// Exact committed checkout and the main revision over which it is qualified. DATA only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub at: String,
    pub base: String,
}

/// The host's actual qualification outcome. Only the host can supply `Q`.
pub enum Judgment<Q> {
    Passed(Q),
    Exit(i32),
    Interrupted(String),
    /// The host cannot qualify or recheck this revision in this image: the loop yields the
    /// step's [`Checkpoint`] ([`Outcome::Elsewhere`]) for another image to [`resume`].
    Elsewhere,
}

/// Which step a [`Checkpoint`] stopped before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A whole qualification of the revision.
    Qualify,
    /// A source recheck of the revision, a merge a composition made.
    Recheck,
}

/// Where the loop stopped for another image: the step it had not begun, its revision, the
/// re-judges spent before it and, before a recheck, the compositions spent since the last
/// whole qualification. DATA only: it names no qualification and grants no publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub step: Step,
    pub revision: Revision,
    pub rejudges: usize,
    pub composes: usize,
}

/// How [`run`] or [`resume`] ended: the exit, or a step the host yielded to another image
/// with the host's own push session (a recheck's; a qualification carries none).
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<S> {
    Exit(i32),
    Elsewhere {
        checkpoint: Checkpoint,
        session: Option<S>,
    },
}

/// A host-controlled checkout merge.
pub enum Merge {
    Clean,
    Exit(i32),
}

/// The host's composition decision, never a source or check owner.
pub enum Composition {
    Composes { tree: String, why: String },
    Rechecks { tree: String, shared: Vec<String> },
    Doomed(String),
}

/// Read the actual running judgment's end/red state; this is never a verdict.
pub type Moot<'m> = &'m mut dyn FnMut() -> bool;

/// Per-judgment main observations. The host supplies admitted reads and composition policy,
/// and drives polls at its actual stage boundaries and finite polling interval.
pub struct Watch {
    composed: String,
    failed: Option<String>,
    doomed: Option<(String, String)>,
}
impl Watch {
    pub fn new(base: &str) -> Self {
        Self {
            composed: base.into(),
            failed: None,
            doomed: None,
        }
    }
    /// A doomed tip requests interruption, never grants or invalidates a completed owner.
    /// An actual run that ends while a read is pending still decides by its own outcome.
    pub fn poll(
        &mut self,
        moot: Moot<'_>,
        refresh: &mut dyn FnMut(Moot<'_>) -> Result<String, String>,
        composition: &mut dyn FnMut(&str) -> Result<Composition, String>,
        failed: &mut dyn FnMut(&str),
    ) -> bool {
        if self.doomed.is_some() {
            return true;
        }
        let read = refresh(&mut *moot).and_then(|tip| {
            if tip == self.composed {
                return Ok(None);
            }
            match composition(&tip)? {
                Composition::Composes { .. } | Composition::Rechecks { .. } => {
                    self.composed = tip;
                    Ok(None)
                }
                Composition::Doomed(why) => Ok(Some((tip, why))),
            }
        });
        match read {
            Ok(None) => {
                self.failed = None;
                false
            }
            Ok(Some(found)) => {
                self.doomed = Some(found);
                true
            }
            Err(_) if moot() => false,
            Err(why) => {
                if self.failed.as_ref() != Some(&why) {
                    failed(&why);
                }
                self.failed = Some(why);
                false
            }
        }
    }
    /// The observed tip/reason that requested a cut. DATA, not proof the run interrupted.
    pub fn into_doomed(self) -> Option<(String, String)> {
        self.doomed
    }
}

/// One admitted publication operation. An unknown write is an error, not `Moved`.
pub enum Publication {
    Done(i32),
    Moved(String),
}

/// Diagnostics about transitions; none is an execution or publication receipt.
pub enum Event<'a> {
    CheckoutMoved(&'a str),
    MainMoved(&'a str),
    Composing {
        tip: &'a str,
        why: &'a str,
    },
    RecheckNeeded {
        tip: &'a str,
        shared: &'a [String],
    },
    DoesNotCompose {
        tip: &'a str,
        why: &'a str,
    },
    MergeDiffers {
        tip: &'a str,
        made: &'a str,
        expected: &'a str,
    },
    StoppedComposes {
        base: &'a str,
        limit: usize,
    },
    StoppedRejudges {
        base: &'a str,
        limit: usize,
        interrupted: bool,
    },
    StoppedUnjudged {
        tip: &'a str,
        limit: usize,
    },
}

/// Finite orchestration bounds. Actual execution/network deadlines stay with their owners.
pub struct Limits {
    pub rejudges: usize,
    pub composes: usize,
    pub stopped: i32,
    pub unjudged: i32,
}

/// Borrow the same opaque qualification and push session for one admitted host operation.
/// The revision/tree arguments are DATA; they do not create source or remote authority.
pub struct PushRequest<'a, Q, S> {
    pub qualified: &'a Q,
    pub revision: &'a Revision,
    pub onto: &'a str,
    pub composed_tree: Option<&'a str>,
    pub session: &'a mut S,
    pub budget: &'a mut Budget,
}

/// A shared obstruction/composition budget, monotone within one qualification epoch.
/// Only the engine resets it after a new whole qualification; a source-only recheck does
/// not replenish it. Actual admission and absolute deadlines remain the host's owners.
pub struct Budget {
    used: usize,
    limit: usize,
}
impl Budget {
    /// Import a caller's existing count, refusing a count outside its original bound.
    fn with_used(limit: usize, used: usize) -> Result<Self, String> {
        if used > limit {
            return Err("landing race count exceeds its original bound".into());
        }
        Ok(Self { used, limit })
    }
    pub fn used(&self) -> usize {
        self.used
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
    pub fn exhausted(&self) -> bool {
        self.used == self.limit
    }
    pub fn consume(&mut self) -> bool {
        if self.exhausted() {
            return false;
        }
        self.used += 1;
        true
    }
}

/// All effects and policy stay with the host. In particular, `publish` must revalidate its
/// actual qualified owner, current source, required check population and expected remote
/// tip through its admitted transport. These DATA arguments grant none of those owners.
pub trait Host {
    type Qualification;
    type PushSession;
    fn qualify(&mut self, revision: &Revision) -> Result<Judgment<Self::Qualification>, String>;
    fn recheck(&mut self, revision: &Revision) -> Result<Judgment<Self::Qualification>, String>;
    fn current(&self, at: &str) -> Result<Option<String>, String>;
    fn merge(&mut self, tip: &str) -> Result<Merge, String>;
    fn head(&self) -> Result<String, String>;
    fn tree(&self, at: &str) -> Result<String, String>;
    fn composition(&self, revision: &Revision, tip: &str) -> Result<Composition, String>;
    /// Retain whatever the host holds across this publication sequence: its admission, and an
    /// absolute deadline where the host sets one. Repeated composition and pre-send
    /// obstructions reuse this SAME session; a host that bounds each remote step instead, and
    /// the sequence by this loop's budget alone, holds nothing in it.
    fn begin_push(&mut self) -> Result<Self::PushSession, String>;
    fn publish(
        &mut self,
        request: PushRequest<'_, Self::Qualification, Self::PushSession>,
    ) -> Result<Publication, String>;
    fn event(&mut self, event: Event<'_>);
}

enum Pushing {
    Done(i32),
    Moved(String),
    Recheck { tip: String, tree: String },
}

/// Run the one composition/requalification loop. The host already established the initial
/// committed source and merged main. This loop saves no result and reads none; a host may answer
/// the first [`Host::qualify`] from its own kept record of a qualification it passed on that same
/// revision (aterm-verify's `land_engine` states its exception and trust boundary), and is then
/// bound by every rule here as for one it just ran. A step the host yields
/// ([`Judgment::Elsewhere`]) ends the loop with its [`Checkpoint`], taken before that step began.
pub fn run<H: Host>(
    host: &mut H,
    revision: Revision,
    limits: Limits,
) -> Result<Outcome<H::PushSession>, String> {
    let mut budget = Budget::with_used(limits.composes, 0)?;
    let mut session = None;
    let judging = qualify(host, &revision, &mut budget, &mut session)?;
    drive(host, revision, 0, budget, session, judging, &limits)
}

/// Whether this image may resume `checkpoint` ([`resume`]), with a push session carried
/// across (`session`) or without one: refused past this image's re-judge or composition bound,
/// with a session a qualification never carries or without the one a recheck needs, or with a
/// composition count before a qualification. [`resume`] admits its checkpoint so itself; a
/// host asks first only to tell such a refusal from what the resumed step then does.
///
/// # Errors
/// Why the checkpoint is refused.
pub fn admit(checkpoint: &Checkpoint, session: bool, limits: &Limits) -> Result<(), String> {
    if checkpoint.rejudges > limits.rejudges {
        return Err(format!(
            "the landing was handed over at re-judge {}, past this image's bound of {}",
            checkpoint.rejudges, limits.rejudges
        ));
    }
    Budget::with_used(limits.composes, checkpoint.composes)?;
    match (checkpoint.step, session) {
        (Step::Qualify, false) if checkpoint.composes == 0 => Ok(()),
        (Step::Qualify, false) => Err("a qualify checkpoint carries no composition count".into()),
        (Step::Qualify, true) => Err("a qualify checkpoint carries no push session".into()),
        (Step::Recheck, false) => Err("a recheck checkpoint carries no push session".into()),
        (Step::Recheck, true) => Ok(()),
    }
}

/// Continue the loop another image of this host yielded ([`Outcome::Elsewhere`]) at its
/// `checkpoint`, with the push session the host carried across (a recheck's only). The
/// checkpoint is refused as [`admit`] refuses it, and the checkout is read before its step
/// runs: one that left the revision ends the landing as unjudged. As at [`run`]'s first
/// qualification, the host may answer that step ([`Host::qualify`] or [`Host::recheck`]) from
/// its own kept record of one it passed on that same revision, and is then bound by every rule
/// here as for one it just ran.
pub fn resume<H: Host>(
    host: &mut H,
    checkpoint: Checkpoint,
    session: Option<H::PushSession>,
    limits: Limits,
) -> Result<Outcome<H::PushSession>, String> {
    admit(&checkpoint, session.is_some(), &limits)?;
    let Checkpoint {
        step,
        revision,
        rejudges,
        composes,
    } = checkpoint;
    let mut budget = Budget::with_used(limits.composes, composes)?;
    if left(host, &revision.at)? {
        return Ok(Outcome::Exit(limits.unjudged));
    }
    match (step, session) {
        (Step::Qualify, _) => {
            let mut session = None;
            let judging = qualify(host, &revision, &mut budget, &mut session)?;
            drive(host, revision, rejudges, budget, session, judging, &limits)
        }
        (Step::Recheck, None) => Err("a recheck checkpoint carries no push session".into()),
        (Step::Recheck, Some(session)) => match host.recheck(&revision)? {
            Judgment::Exit(code) => Ok(Outcome::Exit(code)),
            Judgment::Passed(q) => drive(
                host,
                revision,
                rejudges,
                budget,
                Some(session),
                Judgment::Passed(q),
                &limits,
            ),
            Judgment::Interrupted(_) => Err(UNPOLLED_RECHECK.into()),
            Judgment::Elsewhere => Ok(Outcome::Elsewhere {
                checkpoint: Checkpoint {
                    step: Step::Recheck,
                    revision,
                    rejudges,
                    composes: budget.used(),
                },
                session: Some(session),
            }),
        },
    }
}

/// Whether the checkout left `at` (said as [`Event::CheckoutMoved`]).
fn left<H: Host>(host: &mut H, at: &str) -> Result<bool, String> {
    match host.current(at)? {
        Some(why) => {
            host.event(Event::CheckoutMoved(&why));
            Ok(true)
        }
        None => Ok(false),
    }
}

/// What a recheck that reports an interruption is: none of its runs is polled.
const UNPOLLED_RECHECK: &str = "a source recheck was interrupted without a landing poll";

/// The loop from a qualification's outcome `judging` of `revision`, with `rejudges` spent
/// and the epoch's `budget` and push `session`.
fn drive<H: Host>(
    host: &mut H,
    mut revision: Revision,
    mut rejudges: usize,
    mut budget: Budget,
    mut session: Option<H::PushSession>,
    mut judging: Judgment<H::Qualification>,
    limits: &Limits,
) -> Result<Outcome<H::PushSession>, String> {
    loop {
        let (tip, cut) = match judging {
            Judgment::Exit(code) => return Ok(Outcome::Exit(code)),
            // Every qualification reaches here with its revision and re-judge count set.
            // A whole qualification starts a new epoch, so none carries a session.
            Judgment::Elsewhere => {
                return Ok(Outcome::Elsewhere {
                    checkpoint: Checkpoint {
                        step: Step::Qualify,
                        revision,
                        rejudges,
                        composes: 0,
                    },
                    session: None,
                });
            }
            Judgment::Interrupted(tip) => (tip, true),
            Judgment::Passed(qualified) => {
                match push(
                    host,
                    &qualified,
                    &revision,
                    &mut budget,
                    &mut session,
                    limits,
                )? {
                    Pushing::Done(code) => return Ok(Outcome::Exit(code)),
                    Pushing::Moved(tip) => (tip, false),
                    Pushing::Recheck { tip, tree } => {
                        if left(host, &revision.at)? {
                            return Ok(Outcome::Exit(limits.unjudged));
                        }
                        if let Merge::Exit(code) = host.merge(&tip)? {
                            return Ok(Outcome::Exit(code));
                        }
                        let at = host.head()?;
                        let made = host.tree(&at)?;
                        if made == tree {
                            let rechecked = Revision { at, base: tip };
                            match host.recheck(&rechecked)? {
                                Judgment::Exit(code) => return Ok(Outcome::Exit(code)),
                                Judgment::Passed(q) => {
                                    revision = rechecked;
                                    judging = Judgment::Passed(q);
                                    continue;
                                }
                                Judgment::Interrupted(_) => {
                                    return Err(UNPOLLED_RECHECK.into());
                                }
                                Judgment::Elsewhere => {
                                    return Ok(Outcome::Elsewhere {
                                        checkpoint: Checkpoint {
                                            step: Step::Recheck,
                                            revision: rechecked,
                                            rejudges,
                                            composes: budget.used(),
                                        },
                                        session: session.take(),
                                    });
                                }
                            }
                        }
                        host.event(Event::MergeDiffers {
                            tip: &tip,
                            made: &made,
                            expected: &tree,
                        });
                        if rejudges >= limits.rejudges {
                            host.event(Event::StoppedUnjudged {
                                tip: &tip,
                                limit: limits.rejudges,
                            });
                            return Ok(Outcome::Exit(limits.stopped));
                        }
                        revision = Revision { at, base: tip };
                        rejudges += 1;
                        judging = qualify(host, &revision, &mut budget, &mut session)?;
                        continue;
                    }
                }
            }
        };
        if left(host, &revision.at)? {
            return Ok(Outcome::Exit(limits.unjudged));
        }
        if rejudges >= limits.rejudges {
            host.event(Event::StoppedRejudges {
                base: &revision.base,
                limit: limits.rejudges,
                interrupted: cut,
            });
            return Ok(Outcome::Exit(limits.stopped));
        }
        if let Merge::Exit(code) = host.merge(&tip)? {
            return Ok(Outcome::Exit(code));
        }
        revision = Revision {
            at: host.head()?,
            base: tip,
        };
        rejudges += 1;
        judging = qualify(host, &revision, &mut budget, &mut session)?;
    }
}

fn qualify<H: Host>(
    host: &mut H,
    revision: &Revision,
    budget: &mut Budget,
    session: &mut Option<H::PushSession>,
) -> Result<Judgment<H::Qualification>, String> {
    let judged = host.qualify(revision)?;
    if matches!(judged, Judgment::Passed(_)) {
        // A new whole qualification starts an epoch. Source-only rechecks
        // never pass through here and retain the actual original push session.
        budget.used = 0;
        *session = None;
    }
    Ok(judged)
}

fn push<H: Host>(
    host: &mut H,
    qualified: &H::Qualification,
    revision: &Revision,
    budget: &mut Budget,
    original_session: &mut Option<H::PushSession>,
    limits: &Limits,
) -> Result<Pushing, String> {
    let mut onto = revision.base.clone();
    let mut tree: Option<String> = None;
    if original_session.is_none() {
        *original_session = Some(host.begin_push()?);
    }
    let session = original_session
        .as_mut()
        .ok_or("landing lost its actual push session")?;
    loop {
        let tip = match host.publish(PushRequest {
            qualified,
            revision,
            onto: &onto,
            composed_tree: tree.as_deref(),
            session,
            budget,
        })? {
            Publication::Done(code) => return Ok(Pushing::Done(code)),
            Publication::Moved(tip) => tip,
        };
        host.event(Event::MainMoved(&tip));
        let composition = host.composition(revision, &tip)?;
        if !matches!(composition, Composition::Doomed(_)) && !budget.consume() {
            host.event(Event::StoppedComposes {
                base: &revision.base,
                limit: limits.composes,
            });
            return Ok(Pushing::Done(limits.stopped));
        }
        match composition {
            Composition::Composes { tree: made, why } => {
                host.event(Event::Composing {
                    tip: &tip,
                    why: &why,
                });
                tree = Some(made);
                onto = tip;
            }
            Composition::Rechecks { tree, shared } => {
                host.event(Event::RecheckNeeded {
                    tip: &tip,
                    shared: &shared,
                });
                return Ok(Pushing::Recheck { tip, tree });
            }
            Composition::Doomed(why) => {
                host.event(Event::DoesNotCompose {
                    tip: &tip,
                    why: &why,
                });
                return Ok(Pushing::Moved(tip));
            }
        }
    }
}

/// An actual transport's outcome. Only its exact pre-send obstruction may retry.
pub enum Attempt<I> {
    Done,
    Rejected(String),
    Blocked { intent: I, why: String },
}
pub enum Observation {
    Landed,
    Unchanged,
    Moved(String),
}

/// Retain the original transport/session/deadline in the callbacks. Every obstruction
/// consumes the same budget as composition; unknown or rejected writes never retry.
pub fn push_after_intents<I>(
    budget: &mut Budget,
    attempt: &dyn Fn() -> Result<Attempt<I>, String>,
    wait: &dyn Fn(&I) -> Result<(), String>,
    observe: &dyn Fn() -> Result<Observation, String>,
    refused: &dyn Fn(&str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    loop {
        match attempt()? {
            Attempt::Done => return Ok(None),
            Attempt::Rejected(why) => return refused(&why),
            Attempt::Blocked { intent, why } => {
                if !budget.consume() {
                    return Err(format!(
                        "{why}; stopped after {} push/composition races",
                        budget.limit()
                    ));
                }
                wait(&intent)?;
                match observe()? {
                    Observation::Landed => return Ok(None),
                    Observation::Moved(tip) => return Ok(Some(tip)),
                    Observation::Unchanged => {}
                }
            }
        }
    }
}

/// Adapt an existing host's persisted count without giving a publication callback a new
/// budget constructor. The same shared loop updates the count on success and refusal.
pub fn push_after_intents_with_count<I>(
    limit: usize,
    count: &mut usize,
    attempt: &dyn Fn() -> Result<Attempt<I>, String>,
    wait: &dyn Fn(&I) -> Result<(), String>,
    observe: &dyn Fn() -> Result<Observation, String>,
    refused: &dyn Fn(&str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    let mut budget = Budget::with_used(limit, *count)?;
    let result = push_after_intents(&mut budget, attempt, wait, observe, refused);
    *count = budget.used();
    result
}

#[cfg(test)]
mod tests;
