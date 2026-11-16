// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified verify land` (design B10, `docs/DESIGN-fast-build-release-2026-11-05.md`
//! §4.6): land this checkout's branch on main, judged.
//!
//! 1. Fetch main and merge it (M1). A conflict whose every path has a mechanical rule
//!    ([`resolve`]) is resolved and committed, the rules named in its message, and that merge is
//!    judged as any other; otherwise the landing stops with [`exit::CONFLICT`], the merge left
//!    for the person. The same rule owns each re-judge's merge (step 4). A change's changelog
//!    entry is its own file under `changes/`, so two branches' entries are disjoint paths and
//!    need no merge of their own. M1 is made by the verifier the branch's own tree builds, so a
//!    branch older than these rules still stops on any conflict there, with [`exit::FAILED`].
//!    The landing then applies its fixes ([`autofix`]: the open-issues rule's, which holds
//!    `docs/KNOWN_ISSUES.md` to the entry-level merge of what each merge on the branch merged,
//!    where Git's union of its lines kept an entry one side closed, put one entry's heading over
//!    another's text or filed one entry's lines under another's heading ([`issues`]), then
//!    `census --fix`'s and the formatter's over what HEAD holds unlike main) and commits what
//!    they wrote into that merge, or, where the branch already holds main and nothing was
//!    merged, as a commit of its own; each re-judge's merge is fixed the same way, and a merge a
//!    person made into a kept judgment by the open-issues rule. So fmt and census judge the fixed
//!    tree, and a file a fix wrote is in the change since main, its crate in the cone.
//! 2. Run the dev set over the cone since M1 (`verify --base M1`, B7's export scan included),
//!    on the committed checkout: one that changes during the run pushes nothing. The run's
//!    first red ends it (`--fail-fast`), and what that red stopped is named
//!    ([`Decision::stopped`]). Its events go to its events file alone
//!    ([`crate::cli::EVENTS_FILE_ONLY`]), and the landing ends with one verdict block
//!    ([`verdict`]). While it runs,
//!    main is read at each stage boundary the run's events report and at least every [`POLL`]
//!    (`docs/DESIGN-landing-economy.md` §2.0). The first tip against which nothing the run
//!    would produce can stand (step 4) interrupts the run, ending its group
//!    ([`interrupt_judgment`]): the interrupted run is no verdict, and step 4's merge and
//!    re-judge follow at once. A tip that composes, that a round answers or whose conflict
//!    keeps the judgment (step 4) changes nothing: the run finishes and stands for the tree it
//!    ran on. Neither does a read of a tip the last one that left the run going already holds
//!    (a stale read, or a main rewound by force: [`aterm_land::Watch::poll`]), which is said
//!    once; a main that stays rewound refuses the push. Once the run reports a red (a
//!    check, a stage or a test binary with no pass), main is not read again: its end decides,
//!    as without the poll.
//! 3. Every red, whatever main's own state, and a run whose record is not whole, exits
//!    non-zero ([`decide`]), and nothing is pushed.
//! 4. Push (a push git reports failed whose landing main holds landed). Where main moved to M2
//!    meanwhile, compose the pass onto M2 when AGENTS.md's rule holds: a clean merge and
//!    disjoint endpoint changes after discarding only shared paths with identical mode,
//!    type and object (or all absent) in checked, new main and merge, including parent trees.
//!    Unavailable entry evidence keeps the original overlap. A sole remaining shared
//!    `Cargo.lock` may instead carry an isolated local product-version move
//!    ([`version::independent_lock`]). The four immutable trees retain every other lock
//!    byte and the source-proven binary-leaf identity boundary. That
//!    is seconds of plumbing, so a main that moves again is composed onto again at once
//!    ([`COMPOSES`] in a row at most since a judgment or round passed). That count is the push's bound: no
//!    deadline spans the landings of one judgment, and each remote step runs under its own hang
//!    detector ([`NETWORK`]; a write under [`WRITE`]). A remote step that fails or times out
//!    having sent nothing (a read of main, a push agh says it did not send) is made again after
//!    a doubling pause, [`RETRIES`] in a row at most; a push whose outcome is unknown is never
//!    made again, and a main that holds its landing took it. Where the endpoints share any
//!    other path and the merge is clean, THE ROUND: merge M2 and judge the merge (L), as L's own
//!    freshly built verifier, with the dev set since M2 over the crates of the branch's cone
//!    that main's change reaches (`crate::changed::resolve_round`): of the crates a judgment
//!    of L since M2 would select, those the cone of the shared paths holds ([`Git::shared`],
//!    [`Delta`]: each path of the branch's or of main's change that L holds unlike that side,
//!    and each it holds unlike both) and those the cone of L since the judged commit holds,
//!    which is what main changed, shared with the branch or not. A crate of the branch's cone
//!    left out builds from the judged tree's own files, so the judgment answers for it. The
//!    source readers ([`RECHECKS`]) read the branch's whole delta since M2, and the cross
//!    cells close over the judgment's own seeds. A round that passes is recorded as
//!    `Aterm-Rechecked` and the merge is pushed (composed onto again as any judgment is); a red
//!    counts as in a judgment (step 3), and a merge whose
//!    tree is not the one the rule tested is judged in full. Shared Markdown no build, include
//!    or test reads (root `*.md`, `docs/**/*.md`, a member's `SPECS.md`:
//!    [`aterm_census::no_reach`]) is in no crate's cone, so it must be a regular file on both
//!    sides and in the merge ([`Git::no_reach_holds`]); a round
//!    of the landing's own merge whose shared paths are all such Markdown, which no file may read,
//!    judges no crate, as a disjoint composition judges none (a merge a person made of them
//!    is judged over the crates main's change reaches, as any other round). Such Markdown
//!    that a file may read ([`Git::readers`]: the land-time search
//!    [`aterm_census::no_reach::may_read`], and for a spec table every string that can end
//!    one's path, [`walks_spec_tables`]) changes a verdict only through such a file, so it
//!    stands for those files ([`Delta::readers`]) as a shared path a crate owns stands for
//!    itself: the round judges, within the branch's cone, each one's member, with its
//!    dependents unless only that member's tests, benches and examples compile the file, and
//!    what main's change reaches (`crate::changed::resolve_round`). Where the Markdown rule
//!    does not hold (no regular file on every side, a merge with another no-reach rule, a
//!    reader no workspace member owns), where the round would judge every crate the branch's
//!    change reaches (what main changed, with those readers, reaches them all or none of
//!    them, or the branch's change reaches every crate: [`Seams::reach`],
//!    [`reaches_all`]), or where M2 does not descend from M1,
//!    nothing a judgment of the branch produced stands: merge M2 and judge the branch's
//!    delta from M2 against its new dependencies (`verify --base M2`). No count of re-judges
//!    ends the landing: each merges the main tip that caused it, so none judges one tip twice,
//!    and what ends it is a red, a conflict, a checkout that moved, [`COMPOSES`] push races,
//!    [`RETRIES`] remote failures or a hang detector. Semantic breaks across disjoint files are
//!    B8's. Where the merge of M2 CONFLICTS, the judgment stands for the tree it ran on all the
//!    same ([`Composition::Held`]): a running one finishes, it is kept (step 6), and the merge
//!    stops on the conflict for the person, unless the paths that merge is bound to share
//!    already leave nothing standing ([`conflicting`]). The merge they commit is judged by a
//!    round like the tool's own ([`Git::kept_merge`]), with two differences where its tree is not
//!    the one the landing's own merge makes ([`Delta::mechanical`]): no shared path's crate is
//!    judged alone, since a main change the merge dropped reads as no change beside the judged
//!    tree; and one that shares a lock, a root manifest, a toolchain or lint file or a submodule's
//!    entry is judged in full, as is any merge that shares Markdown a file may read
//!    ([`round_delta`]).
//! 5. The landing merge states what ran ([`aterm_land_trailer`]), an interrupted judgment
//!    as interrupted. A branch whose head already merges the main it lands on is that merge,
//!    restated with the trailers.
//! 6. A judgment that passed is kept in the worktree ([`KEPT`]) until its landing is on main, so
//!    no ending between the pass and the push loses it: the next run whose HEAD is the judged
//!    commit, holding the judged tree, starts at step 4 with that receipt, and judges only what
//!    step 4's rule says it must. A next run whose HEAD merges a main commit into the judged
//!    commit (its first parent the judged commit, its second a commit of main that descends
//!    from the main the judgment merged) starts at the round of that merge: the merge a person
//!    committed after a conflict, or the tool's own after a round of it that was killed or
//!    went red. A whole judgment drops the record before it runs, so the tool's own merge
//!    after a re-judge of it that was killed or went red is judged in full again, never by a
//!    round, which judges less.
//! 7. Each judgment's and each round's verdicts are recorded, signed with this Mac's fleet
//!    node key, in the journal's `land/` ([`records`], [`crate::land_record`]), once the run
//!    ended by itself with the checkout still at its commit: every FAIL, and every other row
//!    its plan keyed where its run was whole. Nothing reads them to decide a landing yet: steps
//!    1 to 6 are the same with them as without them.
//!
//! Each judgment and each round runs as the verifier built from the tree it judges: the argv
//! its child gets, that child's parse of it, the reading of its record and every rule after it
//! are one build's. Before each, the landing builds the checkout's verifier; where that build is
//! not this process's own image (sha256, read at the start, before any build), the landing
//! writes a handoff and replaces itself in place with that build as `land --resume <file>`
//! ([`handoff`]). The receiver checks that the checkout still stands at the step's commit,
//! parses the person's `land` words again and resumes the same step with the receipt and the
//! composition count.
//! A hop happens only between steps, never while a judgment, a build or a push runs: it
//! carries no deadline, since none spans a judgment's push (step 4), and no unsettled write,
//! since a write whose outcome is unknown ends the landing. Where step 6's kept judgment
//! already passed the handed step's revision, the receiver does not run it again: it goes to
//! the push with that receipt, as the next run would. A step hops at most [`HOPS`] times: the
//! first receiver was built by the starting image's build recipe, which main may have changed,
//! so its own build of the same tree may be another image, to which it hands the step on once
//! more; the second was built by the tree's own recipe, so a build that is still not itself
//! could not run.
//!
//! A tool, not a gate: nothing refuses a push made without it, and the harness records a landing
//! without its trailers rather than refusing it.

use crate::exit;
use crate::land_run;
use aterm_land_trailer::{Checked, Interrupted, Rechecked, Row, Stated};
use aterm_uds::spawnfd::Command;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "land_engine.rs"]
mod engine;
#[path = "land_handoff.rs"]
pub mod handoff;
#[path = "land_issues.rs"]
mod issues;
#[path = "land_records.rs"]
pub mod records;
#[path = "land_resolve.rs"]
mod resolve;
#[path = "land_tab.rs"]
pub mod tab;
#[cfg(test)]
#[path = "land_tab_tests.rs"]
mod tab_tests;
#[path = "land_version.rs"]
mod version;

pub use handoff::{Handoff, Image};
pub use records::Records;

/// The remote main is fetched from and pushed to.
pub const REMOTE: &str = "origin";
/// The branch the landing moves.
pub const MAIN: &str = "main";
/// How long a running judgment goes at most without main being read, between the stage
/// boundaries its run reports (`docs/DESIGN-landing-economy.md` §2.0).
pub const POLL: Duration = Duration::from_secs(180);
/// How often a running judgment's events are read for a new stage boundary.
const BOUNDARY_TICK: Duration = Duration::from_secs(5);
/// How long an interrupted judgment has to end itself before what is left of its group is
/// killed, and then how long what the group started outside it may go with none of it ending
/// ([`interrupt_judgment`]): its nested verifier ends its stage groups and reports within its own
/// report grace (`exec::group::REPORT_GRACE`), and a parent-death watch gives its child
/// [`aterm_uds::process::END_GRACE`].
const INTERRUPT_GRACE: Duration =
    crate::exec::group::REPORT_GRACE.saturating_add(aterm_uds::process::END_GRACE);
/// How long what an interrupted judgment started outside its group is awaited in all once it was
/// named, [`INTERRUPT_GRACE`] having passed with none of it ending ([`interrupt_judgment`]): a
/// compiler that outlives its judgment still writes the build directory the next one compiles
/// in, so the landing does not judge again beside it.
const UNENDED_BOUND: Duration = Duration::from_secs(120);
/// How many times one step may be handed to another image ([`handoff`]): once by the image
/// that ran the steps before, and once more by that receiver, built from the step's tree by
/// the sender's build recipe, whose own build by the tree's recipe may differ.
pub const HOPS: usize = 2;
/// How many composition and pre-send obstruction races are allowed before the tool stops.
/// A different writer or intent never renews this bound.
pub const COMPOSES: usize = 10;
/// The exit of a landing the tool stopped while main kept moving before its push
/// ([`COMPOSES`]): the branch is merged and judged, not pushed (`EX_TEMPFAIL`, as a yielding
/// `verify watch --sample`).
pub const STOPPED: i32 = 75;
/// The exit of `verify land --tab` whose worker started ([`tab`]): nothing has landed yet, and
/// the worker's task concludes the landing. Not 0, which says a landing landed.
pub const STARTED: i32 = 5;
/// The file in the checkout's own git directory whose lock a landing holds while it runs
/// ([`Git::hold`]), so a second landing of the checkout refuses at once instead of racing it.
pub const LOCK_FILE: &str = "aterm-land.lock";
/// The exit a landing that panicked records ([`concluded`]): the one a Rust panic gives the
/// process, which then dies of it as without the record.
pub const PANICKED: i32 = 101;
/// How many remote failures in a row that sent nothing the push of a passed judgment makes
/// again before the landing stops with its judgment kept: reads of main that failed or timed
/// out, and, counted apart, pushes agh says it did not send
/// ([`crate::remote::Pushed::Unsent`]).
pub const RETRIES: usize = 5;
/// The pause before a failed remote step is made again; each further failure in a row doubles it.
pub const RETRY_PAUSE: Duration = Duration::from_secs(2);
/// The work window on one write of main ([`Wire::write`]): the longest credential grant agh
/// issues ([`agh::git::GRANT_MAX`]). Expiry requests cancellation and retains the native owner
/// through settlement; neither this window nor agh's per-child bound ([`agh::git::GIT_BUDGET`])
/// establishes a deadline for the complete operation's cleanup. Killing the owner before it
/// records its outcome would leave its durable intent open.
const WRITE: Duration = agh::git::GRANT_MAX;
const _: () = assert!(WRITE.as_secs() > agh::git::GIT_BUDGET.as_secs());
/// Where a passed judgment is kept until its landing is on main ([`Git::keep`]): a ref of this
/// worktree alone, which no fetch or push moves.
pub const KEPT: &str = "refs/worktree/aterm/land-judged";

/// The checks every round must have run and passed, whatever its cone holds: the source
/// readers, which read the branch's whole delta since the merged main, and the only checks
/// that read no-reach Markdown (`aterm_census::no_reach`).
pub const RECHECKS: [&str; 3] = ["fmt", "census", "export-scan"];
/// The hang detector on a local git read or write: how long it may go with no output and no
/// CPU or I/O in its process tree ([`Git::complete`], [`Git::status`]), never how long it may
/// take: a git that works slowly on a loaded Mac is waited for, and one that does nothing for
/// this long is ended and named.
const LOCAL: Duration = Duration::from_secs(60);

/// What a stalled git step of the land's own was waited on for, in its error after the step's
/// own words (`git <args>: …`).
const STEP: &str = "this git step";

/// The environment that changes how Git reads every pathspec (under it a `:(literal)` pathspec
/// or a `*.rs` glob matches something else, or nothing), which a person's shell or a calling tool
/// may export: the land's own Git reads none of it.
const PATHSPEC_MODES: [&str; 4] = [
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Where the no-reach rule's source sits in a tree ([`Git::holds_this_rule`]).
const RULE_PATH: &str = "crates/aterm-census/src/no_reach.rs";
/// The hang detector on a git step that reaches the network or checks out a merge: a remote
/// step's wall bound from its start, and for the land's own git ([`Git::complete`],
/// [`Git::status`]) how long it may go with no output and no CPU or I/O in its process tree.
const NETWORK: Duration = Duration::from_secs(600);
/// Where [`Git::update_submodules`] imports a submodule's missing pin: agh's private namespace,
/// one ref in each submodule repository, replaced by each import.
const GITLINK_REF: &str = "refs/agh/verify/gitlink";

/// What the freshly fetched main proves about a push that did not report its landing: after
/// another writer's pre-send obstruction closed, after a write agh did not send, or after a
/// failure or a refusal.
type PushObservation = aterm_land::Observation;

/// The ordinary push and its existing refusal policy are supplied unchanged. Only a typed
/// refusal that sent nothing permits another attempt of the same landing: an exact pre-send
/// obstruction, sharing the composition bound with main movement, and a write agh says it did
/// not send, once `again` allows it ([`unsent_again`]). Main is read before each such attempt.
fn push_after_intents(
    budget: &mut aterm_land::Budget,
    attempt: &dyn Fn() -> Result<crate::remote::Pushed, String>,
    wait: &dyn Fn(&agh::IntentId) -> Result<(), String>,
    observe: &dyn Fn() -> Result<PushObservation, String>,
    refused: &dyn Fn(&str) -> Result<Option<String>, String>,
    again: &dyn Fn(&str) -> Result<(), String>,
) -> Result<Option<String>, String> {
    let moved = std::cell::Cell::new(None);
    aterm_land::push_after_intents(
        budget,
        &|| unsent_again(attempt, again, observe, &moved),
        wait,
        observe,
        &|why| match moved.take() {
            Some(tip) => Ok(Some(tip)),
            None => refused(why),
        },
    )
}

/// One attempt of the shared loop ([`aterm_land::push_after_intents`]): `attempt`'s outcome,
/// where a write agh says it did not send ([`crate::remote::Pushed::Unsent`]) is made again
/// once `again` allows it (its pause waited out) and a fresh read of main (`observe`) finds main
/// where the landing expects it. A main that took the landing is `Done`; one that moved is a
/// refusal whose tip is left in `moved` for the loop's refusal handler. Any other outcome is
/// the attempt's own.
fn unsent_again(
    attempt: &dyn Fn() -> Result<crate::remote::Pushed, String>,
    again: &dyn Fn(&str) -> Result<(), String>,
    observe: &dyn Fn() -> Result<PushObservation, String>,
    moved: &std::cell::Cell<Option<String>>,
) -> Result<aterm_land::Attempt<agh::IntentId>, String> {
    loop {
        return Ok(match attempt()? {
            crate::remote::Pushed::Done => aterm_land::Attempt::Done,
            crate::remote::Pushed::Rejected(why) => aterm_land::Attempt::Rejected(why),
            crate::remote::Pushed::Blocked { intent, why } => {
                aterm_land::Attempt::Blocked { intent, why }
            }
            crate::remote::Pushed::Unsent(why) => {
                again(&why)?;
                match observe()? {
                    PushObservation::Unchanged => continue,
                    PushObservation::Landed => aterm_land::Attempt::Done,
                    PushObservation::Moved(tip) => {
                        moved.set(Some(tip));
                        aterm_land::Attempt::Rejected(why)
                    }
                }
            }
        });
    }
}

#[cfg(test)]
fn push_counting(
    composes: &mut usize,
    attempt: &dyn Fn() -> Result<crate::remote::Pushed, String>,
    wait: &dyn Fn(&agh::IntentId) -> Result<(), String>,
    observe: &dyn Fn() -> Result<PushObservation, String>,
    refused: &dyn Fn(&str) -> Result<Option<String>, String>,
    again: &dyn Fn(&str) -> Result<(), String>,
) -> Result<Option<String>, String> {
    let moved = std::cell::Cell::new(None);
    aterm_land::push_after_intents_with_count(
        COMPOSES,
        composes,
        &|| unsent_again(attempt, again, observe, &moved),
        wait,
        observe,
        &|why| match moved.take() {
            Some(tip) => Ok(Some(tip)),
            None => refused(why),
        },
    )
}

pub use crate::event_log::{CheckRow, Finished, Judged};

/// What a set of changed paths reaches ([`Seams::reach`]): every crate a judgment of them would
/// select, its dependents, readers and namers included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// They reach every crate (a widened cone); the reason.
    Everything(String),
    /// The crates of their cone, by name.
    Crates(BTreeSet<String>),
    /// Their reach was not read (the toolchain did not resolve, the read did not end in time);
    /// the reason. It says nothing either way, so it cuts no judgment and is not kept for the
    /// next tip ([`branch_reach`]).
    Unread(String),
}

/// What a round judges ([`recheck`]): the merge of a moved main into a judged commit. Where the
/// merge is the landing's own ([`Delta::mechanical`]) and each shared path is Markdown no cone
/// holds and no file may read, the source readers alone ([`RECHECKS`]); else the crates a judgment
/// of the merge since the tip would select that the shared paths, the files that may read the
/// shared Markdown or main's change since the judged commit reach
/// (`crate::changed::resolve_round`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    /// The judged commit, the merge's first parent: the tree the kept judgment stands for, and
    /// the one each shared path's files are compared with.
    pub judged: String,
    /// The shared paths ([`Git::shared`]), in byte order.
    pub shared: Vec<String>,
    /// The files of the merge that may read a shared path that is Markdown no cone holds
    /// ([`Git::readers`]), in byte order, each owned by a workspace member: the Markdown stands for
    /// them in the round's selection. None where the merge is not the landing's own: such a merge
    /// is judged in full where a file may read its shared Markdown ([`round_delta`]).
    pub readers: Vec<String>,
    /// Whether the merge's tree is the one the landing's own merge of the two commits makes:
    /// Git's merge, its open-issues file merged by entry ([`Git::landing_tree`]). Where it is not
    /// (a conflict resolved, or the merge edited, by hand; or a merge Git made whose open-issues
    /// file that rule changes) a shared path may be one the merge holds as the judged commit
    /// does, a change of main's it dropped: its file is then no different from the judged
    /// tree's, so no member is judged alone on that comparison ([`crate::cli::BY_HAND`]).
    pub mechanical: bool,
}

/// What a run's reds come to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// The checks whose reds count (and a failed run that named no failed check).
    pub counted: Vec<String>,
    /// The checks that did not run, and a run that did not finish or whose record is not whole.
    pub not_run: Vec<String>,
    /// The checks the run's first red kept from running (`--fail-fast`): beside a counted red
    /// they decide nothing; with none, each is in [`Self::not_run`] instead.
    pub stopped: Vec<String>,
}

/// What a run's rows come to: every failed check counts, whatever main's own state; anything
/// that did not run, and a record not whole ([`Judged::whole`]), is not a pass.
#[must_use]
pub fn decide(judged: &Judged) -> Decision {
    let mut decision = Decision::default();
    for row in &judged.checks {
        match row.word.as_str() {
            "ok" | "skip" => {}
            "FAIL" => decision.counted.push(row.check.clone()),
            _ if row.stopped => decision.stopped.push(row.check.clone()),
            _ => decision.not_run.push(row.check.clone()),
        }
    }
    if !judged.whole() {
        decision.not_run.push(
            "the run's record is not whole (its run-finished event is missing or names a check \
             no line holds)"
                .into(),
        );
    }
    match judged.code {
        exit::PASS if judged.checks.is_empty() => {
            decision.not_run.push("the run named no check".into());
        }
        exit::FAILED if decision.counted.is_empty() => {
            decision
                .counted
                .push("the run failed with no failed check named".into());
        }
        exit::COULD_NOT_RUN if decision.not_run.is_empty() => {
            decision.not_run.push("the run could not run".into());
        }
        exit::PASS | exit::FAILED | exit::COULD_NOT_RUN => {}
        code => decision.not_run.push(format!("the run exited {code}")),
    }
    // Beside no counted red, what the run's first red stopped was never judged.
    if decision.counted.is_empty() {
        for check in std::mem::take(&mut decision.stopped) {
            decision
                .not_run
                .push(format!("{check} (stopped by a red that does not count)"));
        }
    }
    decision
}

/// Reads whether a poll's question became moot ([`Poll`]): the run it asks about exited, or
/// reported a red, so its own end decides.
pub type Moot<'m> = aterm_land::Moot<'m>;

/// Asked while a judgment runs whether main's newest tip dooms it: `true` interrupts the run. Its
/// argument reads whether the question became moot meanwhile ([`Moot`]); its read of main ends
/// once it does. A judgment still queued for the judgment turn (`crate::turn`) is asked as one
/// that runs: a tip a round answers cuts neither, since the round after it answers every such tip
/// at once, where a cut of the queued one would pay a merge and a verifier build for each.
pub type Poll<'p> = &'p mut dyn FnMut(Moot<'_>) -> bool;

/// How a judgment's run ([`Seams::judge`]) came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    /// It ended by itself: what it judged.
    Ended(Judged),
    /// A poll's `true` interrupted it ([`interrupt_judgment`]): no verdict, and nothing it wrote
    /// is read.
    Interrupted {
        /// Its wall seconds until it ended, less the time it waited for the judgment turn
        /// (`crate::turn`): what of it judged, so a cut while it was queued counts only its start.
        secs: u64,
        /// Whether it was still queued for the judgment turn when it was cut: it judged nothing.
        queued: bool,
    },
    /// A poll's `true` interrupted it, and processes it had started still ran once the wait for
    /// them ended ([`Ending::Unended`]): no verdict, and nothing judges the checkout again in
    /// this landing, whose build directory they may still write.
    Unended {
        /// Its wall seconds until the wait ended.
        secs: u64,
        /// What still ran.
        left: Vec<Unended>,
    },
}

/// A process an interrupted judgment started outside its process group that still ran when the
/// wait for it ended ([`interrupt_judgment`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unended {
    /// Its pid.
    pub pid: i32,
    /// Its executable's name ([`aterm_uds::runs::exe_word`]), or `?` where that does not read.
    pub word: String,
}

impl std::fmt::Display for Unended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (pid {})", self.word, self.pid)
    }
}

/// `1 process`, `3 processes`.
fn processes(count: usize) -> String {
    format!("{count} process{}", if count == 1 { "" } else { "es" })
}

/// `left` as one phrase: `trustc (pid 41), targo (pid 40)`.
fn unended_words(left: &[Unended]) -> String {
    left.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Apply the available landing repairs to the changed paths of a checkout.
pub type Autofix<'a> = &'a mut dyn FnMut(&Path, &[String]) -> Result<(), String>;

/// What `verify land` calls out to. Production passes [`run`]'s live seams; tests pass scripted
/// ones over scratch repositories.
pub struct Seams<'a> {
    /// A git command in the checkout.
    pub git: &'a dyn Fn(&Path) -> Command,
    /// Build the checkout's verifier as it stands: the image each judgment and recheck runs,
    /// and the one the landing hands itself to where it is not the landing's own
    /// ([`Seams::hand_over`]).
    pub verifier: &'a mut dyn FnMut(&Path) -> Result<Image, String>,
    /// Run the dev set on the checkout (its first argument) as it stands, as the verifier at
    /// the second argument (built by [`Seams::verifier`] from that checkout, the landing's own
    /// image), over the cone since a base commit, to its first red. While it runs it asks its
    /// fourth argument
    /// whether main's newest tip dooms it, at each stage boundary its run reports and at least
    /// every [`POLL`], until the run reports a red (`FAIL`, `NOT RUN`: a check, a stage or a
    /// test binary): its own end then decides, as without the poll.
    /// A `true` interrupts the run ([`interrupt_judgment`], [`Ran::Interrupted`]; where what it
    /// had started did not all end, [`Ran::Unended`]) unless it ended or reported such a red
    /// meanwhile.
    pub judge: &'a mut dyn FnMut(&Path, &Path, &str, Poll<'_>) -> Ran,
    /// Where each judgment's and each round's verdicts ([`records::record`]) and this run's
    /// records ([`Records::run_record`], [`records::finish`]) go: the journal's append, the
    /// clone's spool, this Mac's signer and host, the clock, this run and the interrupt it took.
    pub records: Records<'a>,
    /// What changed paths reach in the checkout as it stands: with no paths, the change since
    /// the base commit (the branch's own reach); else those paths, as changed since it (what
    /// main's tip changed, with the paths it shares with the branch). Asked before a judgment
    /// is cut ([`composition`]): it changes when a judgment restarts, never what a landing
    /// judges.
    pub reach: &'a Reaches<'a>,
    /// Run the dev set on the checkout as it stands, as the verifier at the second argument
    /// (the checkout's own fresh build, the landing's own image), since a base commit, its
    /// crates those of that cone that the delta's shared paths or main's change since the
    /// judged commit reach, with those the files that may read its shared Markdown reach
    /// ([`Delta::readers`]), no shared path's owner judged alone where the merge is not Git's
    /// own ([`Delta::mechanical`]): a round ([`Composition::Rechecks`], or a merge of a kept
    /// judgment, [`Git::kept_merge`]), to its first red. Nothing interrupts it.
    pub recheck: &'a mut dyn FnMut(&Path, &Path, &str, &Delta) -> Judged,
    /// Apply the landing's fixes to the checkout (the first argument) as it stands, over the
    /// paths HEAD holds unlike main (the second): `census --fix`'s and the formatter's
    /// ([`live_autofix`]). An error says what kept them from running whole; what they wrote is
    /// committed all the same ([`autofix`]).
    pub autofix: Autofix<'a>,
    /// Hand the landing to an image (the first argument) with the handoff file it wrote (the
    /// second), in the checkout (the third) ([`handoff::hand_over`]): production's replaces
    /// this process and returns only when that failed, with the exit.
    pub hand_over: &'a mut dyn FnMut(&Image, &Path, &Path) -> i32,
    /// Say a line to the person.
    pub say: &'a mut dyn FnMut(&str),
    /// One remote step of the push phase (each push, and each read of main once a judgment
    /// passed), with an explicit read or mutation lifetime: [`live_remote`] in production.
    pub(crate) remote: &'a RemoteStep<'a>,
    /// Wait out the pause before a failed remote step is made again ([`RETRY_PAUSE`]), and each
    /// tick of a wait on another writer's open intent.
    pub pause: &'a dyn Fn(Duration),
}

/// How the push phase runs one remote step: a git or agh command for the checkout at the path,
/// its git made by the maker, with an explicit lifetime policy and work deadline.
pub(crate) type RemoteStep<'a> = dyn Fn(
        &Path,
        &dyn Fn(&Path) -> Command,
        crate::remote::Tool,
        &[std::ffi::OsString],
        Instant,
        RemoteMode,
    ) -> Result<std::process::Output, String>
    + 'a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteMode {
    Read,
    Mutation,
}

/// Who lands: the person's `land` words, which a handoff carries for the receiver to parse
/// again, and this image's sha256, read before any build.
pub struct Own<'a> {
    /// The person's `land` words.
    pub words: &'a [String],
    /// This process's own image's digest.
    pub image: &'a str,
    /// The commit this landing was asked to land (`--at`, which `--tab` gives its worker): HEAD
    /// that the landing reads must be it, or nothing lands. A resumed landing checked it already.
    pub at: Option<&'a str>,
}

/// What a refused handoff leaves the person to do.
const RERUN: &str = "nothing was pushed. Fix: run `targo --unverified verify land` again: the \
                     checkout holds the merge, committed";
/// What a handoff whose checkout left its commit before the resume leaves the person to do.
const RERUN_MOVED: &str = "nothing was pushed. Fix: run `targo --unverified verify land` again \
                           once the checkout holds what is to land, committed";

/// Land the checkout at `root` ([`Seams`]) as `own`: the exit code ([`exit`], or [`STOPPED`]),
/// once the run's records are written ([`concluded`]). A landing handed to another image
/// returns only where the handoff failed: the receiver ends the run.
pub fn land(root: &Path, own: &Own<'_>, seams: &mut Seams<'_>) -> i32 {
    let git = Git {
        root,
        make: seams.git,
    };
    let mut tally = Tally::default();
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drive(&git, own, seams, &mut tally)
    }));
    concluded(&git, seams, ran, &tally)
}

/// How a landing that ran as `ran` ends, whatever ended it: an error is said (as the interrupt
/// it came after, where the landing took one it has not said) and could not run; then the run's
/// end record and its append ([`records::finish`]), the closing verdict of `tally`
/// ([`say_verdict`]) and the exit. A landing that panicked records its end as [`PANICKED`] and
/// then goes on panicking, so it dies as without the record, and says no verdict.
fn concluded(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    ran: std::thread::Result<Result<i32, String>>,
    tally: &Tally,
) -> i32 {
    let code = match ran {
        Ok(Ok(code)) => code,
        Ok(Err(why)) => {
            match (seams.records.interrupt)() {
                Some(signal) if !seams.records.tally.stopped => {
                    seams.records.tally.stopped = true;
                    (seams.say)(&format!(
                        "verify land: INTERRUPTED by signal {signal}: {why}"
                    ));
                }
                _ => (seams.say)(&format!("verify land: COULD NOT RUN — {why}")),
            }
            exit::COULD_NOT_RUN
        }
        Err(panic) => {
            records::finish(git, &mut seams.records, &mut *seams.say, PANICKED);
            std::panic::resume_unwind(panic);
        }
    };
    records::finish(git, &mut seams.records, &mut *seams.say, code);
    say_verdict(git, seams, code, tally);
    code
}

/// Say the landing's closing verdict ([`verdict`]).
fn say_verdict(git: &Git<'_>, seams: &mut Seams<'_>, code: i32, tally: &Tally) {
    let head = git.head().ok();
    for line in verdict(code, head.as_deref(), tally) {
        (seams.say)(&line);
    }
}

/// Resume, as `own`, the landing of the checkout at `root` another build of this verifier
/// handed over ([`handoff`]): refused, before anything runs, where the handoff names another
/// image or checkout, or a base or cone its commit does not hold. The exit code, as
/// [`land`]'s.
///
/// The run goes on as the one that handed it over: its id, start and next record, and the cause
/// of the step it resumes ([`handoff::Carried`]). A handoff of an earlier schema carries none,
/// so a new run starts here, whose step's cause is `handed` where the receipt holds a judgment.
pub fn resume(root: &Path, own: &Own<'_>, handoff: Handoff, seams: &mut Seams<'_>) -> i32 {
    let git = Git {
        root,
        make: seams.git,
    };
    let records = &mut seams.records;
    match &handoff.run {
        Some(carried) => records.adopt(carried),
        None => {
            let judged =
                !handoff.stated.checked.is_empty() || !handoff.stated.interrupted.is_empty();
            records.tally.next = judged.then(|| land_run::Cause::new("handed", "", ""));
        }
    }
    records.tally.base.clone_from(&handoff.base);
    let mut tally = Tally::default();
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        resume_drive(&git, own, handoff, seams, &mut tally)
    }));
    concluded(&git, seams, ran, &tally)
}

fn resume_drive(
    git: &Git<'_>,
    own: &Own<'_>,
    handoff: Handoff,
    seams: &mut Seams<'_>,
    tally: &mut Tally,
) -> Result<i32, String> {
    let tree = admit(git, own, &handoff).map_err(|why| format!("{why}; {RERUN}"))?;
    let subject = merge_subject(git);
    (seams.say)(&format!(
        "verify land: resumed as {tree}'s own verifier ({}) at the {} of {} over {}",
        short(own.image),
        step_noun(handoff.step),
        handoff.at,
        handoff.base
    ));
    engine::resume(git, seams, own, &subject, handoff, tally)
}

/// Whether `own` may resume `handoff` in the checkout: handed to this image, for this checkout
/// (compared physically), over a base and a cone its commit holds, from a push source the
/// landing admits.
/// The tree of its commit, else why not.
fn admit(git: &Git<'_>, own: &Own<'_>, handoff: &Handoff) -> Result<String, String> {
    if handoff.image != own.image {
        return Err(format!(
            "the landing was handed to image {}, and this verifier is {}",
            short(&handoff.image),
            short(own.image)
        ));
    }
    let physical = |path: &Path| {
        std::fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
    };
    if physical(&handoff.root)? != physical(git.root)? {
        return Err(format!(
            "the landing was handed over for the checkout {}, not {}",
            handoff.root.display(),
            git.root.display()
        ));
    }
    for (what, commit) in [("base", &handoff.base), ("cone", &handoff.cone)] {
        if !git.is_ancestor(commit, &handoff.at)? {
            return Err(format!(
                "the handoff's {what} {commit} is not in its commit {}",
                handoff.at
            ));
        }
    }
    git.admit_push_source()?;
    git.tree(&handoff.at)
}

/// A digest's first twelve digits, as a line names an image.
fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

/// A step as a line names it.
fn step_noun(step: aterm_land::Step) -> &'static str {
    match step {
        aterm_land::Step::Qualify => "judgment",
        aterm_land::Step::Recheck => "recheck",
    }
}

/// The landing merge's subject, by the branch HEAD names.
fn merge_subject(git: &Git<'_>) -> String {
    let label = git
        .local(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|_| "HEAD".into());
    format!("Merge {REMOTE}/{MAIN} into {label}")
}

fn drive(
    git: &Git<'_>,
    own: &Own<'_>,
    seams: &mut Seams<'_>,
    tally: &mut Tally,
) -> Result<i32, String> {
    let say = |seams: &mut Seams<'_>, line: String| (seams.say)(&line);
    let dirty = git.changes()?;
    if !dirty.is_empty() {
        say(
            seams,
            format!(
                "verify land: COULD NOT RUN — the checkout has changes no commit holds:\n{dirty}\n\
                 Fix: commit or remove them, then run it again"
            ),
        );
        return Ok(exit::COULD_NOT_RUN);
    }
    let subject = merge_subject(git);
    let merged = git.fetch()?;
    seams.records.tally.base.clone_from(&merged);
    let start = git.head()?;
    if let Some(at) = own.at {
        let asked = git
            .local(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{at}^{{commit}}"),
            ])
            .unwrap_or_default();
        if asked != start {
            say(
                seams,
                format!(
                    "verify land: COULD NOT RUN — HEAD is {start}, not {at}, the commit this \
                     landing was asked to land ({}): the checkout moved after the ask, and \
                     nothing was landed. Fix: select the intended committed checkout before \
                     running `targo --unverified verify land` again",
                    tab::AT,
                ),
            );
            return Ok(exit::COULD_NOT_RUN);
        }
    }
    if git.is_ancestor(&start, &merged)? {
        let _ = git.forget();
        say(
            seams,
            format!("verify land: nothing to land — HEAD {start} is on {REMOTE}/{MAIN}"),
        );
        return Ok(exit::PASS);
    }
    git.admit_push_source()?;
    // A judgment an earlier run passed and kept, where HEAD is still the commit it ran on: this
    // run starts at its push. Main's tip is then one that moved before the push, as any other.
    let resumed = match git.kept(&start) {
        Ok(None) => None,
        Ok(Some(Ok(kept))) => Some(kept),
        Ok(Some(Err(why))) => {
            // HEAD is not the judged commit. Where it merges a main commit into it, the kept
            // judgment stands for its first parent and a round judges the merge.
            match git.kept_merge(&start, &merged) {
                Ok(Some((kept, onto))) => {
                    say(
                        seams,
                        format!(
                            "verify land: an earlier run judged {} over the cone since {}, and \
                             HEAD merges {onto} into that commit: the merge is judged for what it \
                             changed for the branch",
                            kept.tree, kept.merged
                        ),
                    );
                    if merged != onto {
                        say(
                            seams,
                            format!("verify land: {REMOTE}/{MAIN} has moved to {merged} since"),
                        );
                    }
                    // A merge Git made holds the open-issues file as a union of lines, unlike the
                    // landing's own merge: it is amended, its parents kept, before the round
                    // judges it.
                    let start = hold_to_rule(git, seams, &start, &onto)?.unwrap_or(start);
                    let moved = (merged != onto).then_some(merged);
                    let revision = aterm_land::Revision {
                        at: start,
                        base: onto,
                    };
                    return engine::resume_merged(
                        git, seams, own, &subject, revision, kept, moved, tally,
                    );
                }
                Ok(None) => say(
                    seams,
                    format!("verify land: the kept judgment is not resumed: {why}"),
                ),
                Err(unread) => say(
                    seams,
                    format!(
                        "verify land: the kept judgment is not resumed: {why}; whether HEAD \
                         merges main into the commit it judged was not read ({unread})"
                    ),
                ),
            }
            None
        }
        Err(why) => {
            say(
                seams,
                format!("verify land: the kept judgment was not read ({why}): judging"),
            );
            None
        }
    };
    let (judged_at, merged, moved) = match &resumed {
        Some(kept) => {
            say(
                seams,
                format!(
                    "verify land: resuming at the push: an earlier run judged {} over the cone \
                     since {}, and HEAD is still that commit",
                    kept.tree, kept.merged
                ),
            );
            if merged != kept.merged {
                say(
                    seams,
                    format!("verify land: {REMOTE}/{MAIN} has moved to {merged} since"),
                );
            }
            let moved = (merged != kept.merged).then_some(merged);
            (start, kept.merged.clone(), moved)
        }
        None => {
            let merge = !git.is_ancestor(&merged, &start)?;
            if merge {
                match git.merge(&merged, &subject)? {
                    Merge::Clean { resolved } => {
                        for line in resolved {
                            say(seams, format!("verify land: resolved by rule: {line}"));
                        }
                    }
                    Merge::Conflict { paths, refusals } => {
                        return Ok(conflicted(seams, &merged, &paths, &refusals));
                    }
                }
            }
            autofix(git, seams, &merged, merge)?;
            (git.head()?, merged, None)
        }
    };
    let revision = aterm_land::Revision {
        at: judged_at,
        base: merged,
    };
    engine::drive(git, seams, own, &subject, revision, resumed, moved, tally)
}

/// The subject of the commit the landing's fixes make where it merged nothing ([`autofix`]).
const AUTOFIX_SUBJECT: &str = "Apply the landing's fixes";

/// The most file names a line saying what the landing's fixes wrote lists.
const AUTOFIX_NAMED: usize = 8;

/// Apply the landing's fixes to the checkout: first hold the open-issues file to the entry-level
/// merge of what HEAD's merges merged ([`issues`]), then [`Seams::autofix`] over the
/// paths HEAD holds unlike `main` (deletions aside), and commit what they wrote: into HEAD where
/// it is the merge this landing just made (`merge`), which keeps its parents and message, else
/// as a commit of its own ([`AUTOFIX_SUBJECT`]), the message noting each entry the open-issues
/// rule changed ([`issues::note`]). The judgment that follows judges the fixed tree, its fmt
/// and census rows included, and a file a fix moved or wrote is in the change since `main`, so
/// its crate is in the cone. Fixes that did not run whole are said, and what they wrote is
/// committed all the same: the landing starts only from a clean checkout, so whatever changed
/// is theirs. Nothing changed, nothing is committed. A commit git refuses is an error that
/// names what it left in the checkout.
fn autofix(git: &Git<'_>, seams: &mut Seams<'_>, main: &str, merge: bool) -> Result<(), String> {
    let entries = issues::merge_in_checkout(git, seams, main)?;
    let utf8 = |paths: BTreeSet<Vec<u8>>| -> Vec<String> {
        paths
            .into_iter()
            .filter_map(|path| String::from_utf8(path).ok())
            .collect()
    };
    let changed = utf8(git.paths(&[
        "diff",
        "-z",
        "--name-only",
        "--no-renames",
        "--diff-filter=d",
        main,
        "HEAD",
        "--",
    ])?);
    if changed.is_empty() && entries.is_empty() {
        return Ok(());
    }
    if !changed.is_empty()
        && let Err(why) = (seams.autofix)(git.root, &changed)
    {
        (seams.say)(&format!(
            "verify land: the landing's fixes did not run whole ({why}): fmt and census judge \
             the tree as they left it"
        ));
    }
    let mut wrote = utf8(git.paths(&["diff", "-z", "--name-only", "--ignore-submodules=all"])?);
    wrote.extend(utf8(git.paths(&[
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
    ])?));
    if wrote.is_empty() {
        return Ok(());
    }
    let left = |why: String| {
        format!(
            "{why}. The landing's fixes are left uncommitted in the checkout ({}): commit or \
             remove them, then run it again",
            wrote.join(", ")
        )
    };
    let mut add = vec!["--literal-pathspecs", "add", "--"];
    add.extend(wrote.iter().map(String::as_str));
    git.local(&add).map_err(left)?;
    commit_fixes(git, merge, &entries).map_err(left)?;
    let mut named = wrote
        .iter()
        .take(AUTOFIX_NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if wrote.len() > AUTOFIX_NAMED {
        named.push_str(&format!(" and {} more", wrote.len() - AUTOFIX_NAMED));
    }
    (seams.say)(&format!(
        "verify land: the landing's fixes wrote {} file(s), committed {}: {named}",
        wrote.len(),
        if merge {
            "into the merge"
        } else {
            "as a commit of their own"
        }
    ));
    Ok(())
}

/// Hold the open-issues file of HEAD `at`, a merge this landing did not make or a step handed
/// on to this image, to the rule over main commit `base` ([`issues::merge_in_checkout`]), as
/// this landing's own merge holds it: a person, or an image without the rule, made it as Git
/// merges the file. Where the rule changed the file it is committed into HEAD where HEAD merges
/// `base` (its parents kept, the note after its message: [`commit_fixes`]), else as a commit of
/// its own on HEAD; the new HEAD, `None` where the rule changed nothing.
///
/// # Errors
/// A Git read or the file's write failed, or adding or committing it did, which names the file
/// left uncommitted in the checkout.
fn hold_to_rule(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    at: &str,
    base: &str,
) -> Result<Option<String>, String> {
    let entries = issues::merge_in_checkout(git, seams, base)?;
    if entries.is_empty() {
        return Ok(None);
    }
    let parents = git.local(&["rev-list", "--parents", "--max-count=1", at])?;
    let amend = matches!(
        parents.split_whitespace().collect::<Vec<_>>()[..],
        [_, _, second] if second == base
    );
    git.local(&["--literal-pathspecs", "add", "--", issues::INDEX])
        .and_then(|_| commit_fixes(git, amend, &entries))
        .map_err(|error| {
            format!(
                "{error}: the checkout holds {} as the rule writes it, uncommitted",
                issues::INDEX
            )
        })?;
    let head = git.head()?;
    (seams.say)(&format!(
        "verify land: {at} holds {} as Git merges it: {} {head}",
        issues::INDEX,
        if amend {
            "amended, its parents kept, to"
        } else {
            "the rule's fix is committed on it as"
        }
    ));
    Ok(Some(head))
}

/// Commit what the landing's fixes staged ([`autofix`]): into HEAD where `amend` (its parents
/// kept, and its message, with the note of the `entries` the open-issues rule changed, where it
/// changed any), else as a commit of its own ([`AUTOFIX_SUBJECT`], with that note).
fn commit_fixes(git: &Git<'_>, amend: bool, entries: &[String]) -> Result<(), String> {
    let commit = |args: &[&str]| git.local(args).map(drop);
    match (amend, entries.is_empty()) {
        (true, true) => commit(&["commit", "--quiet", "--no-verify", "--amend", "--no-edit"]),
        (false, true) => commit(&["commit", "--quiet", "--no-verify", "-m", AUTOFIX_SUBJECT]),
        (true, false) => {
            let message = git.local(&["log", "-1", "--format=%B", "HEAD"])?;
            let message = format!("{}\n\n{}", message.trim_end(), issues::note(entries));
            commit(&[
                "commit",
                "--quiet",
                "--no-verify",
                "--amend",
                "-m",
                &message,
            ])
        }
        (false, false) => {
            let message = format!("{AUTOFIX_SUBJECT}\n\n{}", issues::note(entries));
            commit(&["commit", "--quiet", "--no-verify", "-m", &message])
        }
    }
}

/// How a judgment ([`judge`]) came out.
enum Judging {
    /// It passed: its judgment is in the receipt.
    Passed,
    /// The landing ends with this exit.
    Exit(i32),
    /// Main's tip, which does not compose, interrupted it: no verdict, stated as interrupted.
    Interrupted(String),
    /// It did not run: the tree's own verifier is this image, which the landing goes on as.
    Elsewhere(Image),
}

/// How a round ([`recheck`]) came out.
enum Rechecking {
    /// It passed: its round is in the receipt.
    Passed,
    /// The landing ends with this exit.
    Exit(i32),
    /// It did not run: the merge's own verifier is this image, which the landing goes on as.
    Elsewhere(Image),
    /// It did not run: no round answers for the merge (said), which is judged in full.
    Whole,
}

/// The record at [`KEPT`] as read ([`Git::kept_record`]): the commit it judged with the
/// judgment, or why the record is not whole.
type KeptRecord = Result<(String, Kept), String>;

/// What the merge a round was asked of comes to ([`round_delta`]).
enum Rounded {
    /// A round judges it, over this delta; `markdown` where every shared path is Markdown no
    /// cone holds and no file of the merge may read, which the source readers alone answer for
    /// (a landing of kind `markdown`, [`crate::land_run::Landing::kind`]).
    Round { delta: Delta, markdown: bool },
    /// No round answers for it, for this reason: it is judged in full over the cone since the
    /// tip it merged.
    Whole(String),
}

/// What the landing holds between its steps.
struct Held<'o> {
    /// The main commit the landing's first judgment merged, which a handoff carries.
    cone: String,
    /// The receipt so far.
    stated: Stated,
    /// The branch's reach over the cone since a main commit, as last read ([`Seams::reach`]):
    /// one read answers every poll of a judgment over that commit.
    reach: std::cell::RefCell<Option<(String, Reach)>>,
    /// This image's digest.
    own: &'o str,
    /// The handoffs the step about to run has taken: a resumed step's [`Handoff::hop`], 0 for
    /// one this image came to itself. A step that yields hands over as hop one more.
    hop: usize,
    /// What the landing's closing verdict reads ([`verdict`]).
    tally: Tally,
}

/// What the landing's closing verdict ([`verdict`]) reads: the last judgment or round this
/// process ran to its end, and whether its landing reached main.
#[derive(Debug, Default)]
struct Tally {
    /// The last judgment or round that ran here to its end.
    last: Option<Last>,
    /// The landing commit, once main holds it.
    landed: Option<String>,
}

/// A judgment or a round this process ran to its end, as the verdict names it.
#[derive(Debug)]
struct Last {
    /// A round's, else a judgment's.
    round: bool,
    /// The tree it judged.
    tree: String,
    /// The main commit it judged since.
    base: String,
    /// What it judged.
    judged: Judged,
    /// What its reds came to.
    decision: Decision,
}

/// What an exit means, in a verdict's words.
fn meaning(code: i32) -> &'static str {
    match code {
        exit::PASS => "passed, and this run pushed nothing",
        exit::FAILED => "a counted red",
        exit::COULD_NOT_RUN => "could not run",
        exit::CONFLICT => "a merge conflict",
        STOPPED => "main kept moving",
        _ => "an unexpected exit",
    }
}

/// The most characters a verdict's list of what did not run takes.
const VERDICT_LIST: usize = 400;

/// The landing's closing verdict, at most nine lines: landed or not, the commit, the last
/// judgment's or round's checks each with its word and time, its first red and where, what its
/// first red stopped, what did not run, and
/// where its full log and events are kept (`verify status --events` reads them). Nothing where
/// nothing ran or landed here: the line that ended the landing says it all. A landing handed to
/// another image says none here, since this process is replaced ([`handoff::hand_over`]); one
/// whose hand-over failed does.
fn verdict(code: i32, head: Option<&str>, tally: &Tally) -> Vec<String> {
    if tally.last.is_none() && tally.landed.is_none() {
        return Vec::new();
    }
    let at = head.map_or_else(String::new, |head| format!("; HEAD {head}"));
    let mut lines = vec![match &tally.landed {
        Some(landing) => format!("verify land: VERDICT — LANDED {landing} on {REMOTE}/{MAIN}"),
        None => format!(
            "verify land: VERDICT — NOT LANDED (exit {code}: {}){at}",
            meaning(code)
        ),
    }];
    let Some(last) = &tally.last else {
        lines.push("  judged by an earlier run, whose kept judgment this run pushed".to_owned());
        return lines;
    };
    lines.push(format!(
        "  the last {} judged {} since {}",
        if last.round { "round" } else { "judgment" },
        last.tree,
        last.base,
    ));
    if !last.judged.checks.is_empty() {
        let stages: Vec<String> = last
            .judged
            .checks
            .iter()
            .map(|row| match row.took {
                Some(took) if row.word != "NOT RUN" => format!(
                    "{} {} {}",
                    row.check,
                    row.word,
                    crate::report::duration(took.as_secs_f64())
                ),
                _ => format!("{} {}", row.check, row.word),
            })
            .collect();
        lines.push(format!("  checks: {}", stages.join(" · ")));
    }
    if let Some(red) = last.judged.reds.first() {
        lines.push(match &red.at {
            Some(at) => format!("  first red: {} at {at}", red.what),
            None => format!("  first red: {}", red.what),
        });
    }
    if !last.decision.stopped.is_empty() {
        lines.push(format!(
            "  stopped at the first red: {} did not run",
            last.decision.stopped.join(", ")
        ));
    }
    if !last.decision.not_run.is_empty() {
        let list = last.decision.not_run.join("; ");
        lines.push(match list.char_indices().nth(VERDICT_LIST) {
            Some((cut, _)) => format!("  did not run: {}…", &list[..cut]),
            None => format!("  did not run: {list}"),
        });
    }
    if let Some(kept) = &last.judged.kept {
        lines.push(format!("  log: {}", kept.log.display()));
        if let Some(events) = &kept.events {
            lines.push(format!(
                "  events: targo --unverified verify status --events {}",
                events.display()
            ));
        }
    }
    lines
}

/// The checkout's verifier, built for a step on `tree` ([`Seams::verifier`]).
enum Built {
    /// It is this image: the step runs here, as it.
    Here(Image),
    /// It is another image: the landing goes on as it.
    Elsewhere(Image),
    /// It did not build (said): the landing ends with this exit.
    Exit(i32),
}

/// Whether the landing took an interrupt ([`Records::interrupt`]), said the first time it is
/// read: the step stops before anything else of it runs (no verdict is read and nothing is
/// pushed), and the run ends with [`exit::COULD_NOT_RUN`], its end record naming the signal it
/// then dies of ([`records::finish`]).
fn stopped(seams: &mut Seams<'_>) -> bool {
    let Some(signal) = (seams.records.interrupt)() else {
        return false;
    };
    if !seams.records.tally.stopped {
        seams.records.tally.stopped = true;
        (seams.say)(&format!(
            "verify land: INTERRUPTED by signal {signal}; nothing was pushed"
        ));
    }
    true
}

/// Build the checkout's verifier for the step at `at` over `base` on `tree`, and compare it
/// with this image. A step that has taken [`HOPS`] handoffs refuses another image, so a build
/// that is not its own image never hands on in a loop; one that runs here leaves the next step
/// none.
fn built(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    (at, base, tree): (&str, &str, &str),
    held: &mut Held<'_>,
) -> Result<Built, String> {
    if stopped(seams) {
        return Ok(Built::Exit(exit::COULD_NOT_RUN));
    }
    let image = match (seams.verifier)(git.root) {
        Ok(image) => image,
        // A build the interrupt stopped is said as the interrupt.
        Err(_) if stopped(seams) => return Ok(Built::Exit(exit::COULD_NOT_RUN)),
        Err(why) => {
            (seams.say)(&format!(
                "verify land: COULD NOT RUN — {tree}'s verifier did not build: {why}; nothing was \
                 pushed"
            ));
            return Ok(Built::Exit(exit::COULD_NOT_RUN));
        }
    };
    if stopped(seams) {
        return Ok(Built::Exit(exit::COULD_NOT_RUN));
    }
    if image.digest == held.own {
        held.hop = 0;
        return Ok(Built::Here(image));
    }
    if held.hop >= HOPS {
        return Err(format!(
            "the image the landing was handed to for {at} over {base} hands it on again: its own \
             build of {tree} ({}) is {}, not itself ({}), after {} handoffs of this step; {RERUN}",
            image.path.display(),
            short(&image.digest),
            short(held.own),
            held.hop
        ));
    }
    Ok(Built::Elsewhere(image))
}

/// A judgment an earlier run kept ([`Git::keep`]) that holds for this checkout ([`Git::kept`]),
/// or for the commit this checkout merged a main commit into ([`Git::kept_merge`]).
struct Kept {
    /// The judged tree, which HEAD holds, or its first parent.
    tree: String,
    /// The main commit the judged commit merged: the base of its last judgment or recheck.
    merged: String,
    /// What ran, as the landing merge will state it.
    stated: Stated,
}

/// The push phase's remote for one landing: each step run through the landing's seam
/// ([`RemoteStep`]) under its own hang detector, from its own start ([`NETWORK`] for a read,
/// [`WRITE`] for a write), and a step that failed having sent nothing made again within
/// [`RETRIES`]. No deadline spans the steps: what bounds a push is those counts and the
/// landing's composition budget ([`COMPOSES`]), so a slow remote or a main that keeps moving
/// never ends a passed judgment by the clock.
struct Wire<'a> {
    git: &'a Git<'a>,
    step: &'a RemoteStep<'a>,
    pause: &'a dyn Fn(Duration),
    say: &'a dyn Fn(&str),
    /// The interrupt the landing took ([`Records::interrupt`]): no failed step is made again
    /// after it, and no wait on another writer goes on.
    interrupt: &'a dyn Fn() -> Option<i32>,
    /// Reads of main that failed in a row.
    reads: std::cell::Cell<usize>,
    /// Pushes agh did not send in a row.
    unsent: std::cell::Cell<usize>,
    /// What a later run must settle first where the push ended with a write whose outcome is
    /// unknown, or with an intent still open: a run before that waits on the intent.
    unsettled: std::cell::RefCell<Option<String>>,
}

impl<'a> Wire<'a> {
    fn new(
        git: &'a Git<'a>,
        step: &'a RemoteStep<'a>,
        pause: &'a dyn Fn(Duration),
        say: &'a dyn Fn(&str),
        interrupt: &'a dyn Fn() -> Option<i32>,
    ) -> Self {
        Self {
            git,
            step,
            pause,
            say,
            interrupt,
            reads: std::cell::Cell::new(0),
            unsent: std::cell::Cell::new(0),
            unsettled: std::cell::RefCell::new(None),
        }
    }

    /// One remote step that writes nothing to main, through the seam: ended as hung
    /// [`NETWORK`] after it starts.
    fn run(
        &self,
        tool: crate::remote::Tool,
        args: &[std::ffi::OsString],
    ) -> Result<std::process::Output, String> {
        (self.step)(
            self.git.root,
            self.git.make,
            tool,
            args,
            Instant::now() + NETWORK,
            RemoteMode::Read,
        )
    }

    /// One mutation with its own [`WRITE`] work window. Expiry or interruption requests native
    /// cancellation; the actual owner runs through settlement before the step returns. Hold
    /// the landing's interrupt watchdog through that cleanup (`exec::group::hold`).
    fn write(
        &self,
        tool: crate::remote::Tool,
        args: &[std::ffi::OsString],
    ) -> Result<std::process::Output, String> {
        let _held = crate::exec::group::hold();
        (self.step)(
            self.git.root,
            self.git.make,
            tool,
            args,
            Instant::now() + WRITE,
            RemoteMode::Mutation,
        )
    }

    /// Allow `what`, which failed for `why` having sent nothing, to be made again: counted in
    /// `failed`, said, and its pause waited out ([`RETRY_PAUSE`], doubled by each failure before
    /// it in a row).
    ///
    /// # Errors
    /// [`RETRIES`] such failures already came in a row, or the landing took an interrupt: `why`,
    /// and that it stopped.
    fn again(&self, failed: &std::cell::Cell<usize>, what: &str, why: &str) -> Result<(), String> {
        if let Some(signal) = (self.interrupt)() {
            return Err(format!(
                "{why}; {what} is not made again: the landing took signal {signal}"
            ));
        }
        let before = failed.get();
        if before == RETRIES {
            return Err(format!(
                "{why}; stopped after {RETRIES} failures of {what} in a row"
            ));
        }
        failed.set(before + 1);
        let pause = RETRY_PAUSE.saturating_mul(1u32 << before);
        (self.say)(&format!(
            "verify land: {what} failed ({why}); nothing was sent, so it is made again in {}s \
             ({} of {RETRIES})",
            pause.as_secs(),
            before + 1
        ));
        (self.pause)(pause);
        Ok(())
    }

    /// Main's tip, fetched into its remote-tracking ref ([`Git::fetch_through`]). A read that
    /// fails or times out changes nothing, so it is made again ([`Self::again`]).
    fn main(&self) -> Result<String, String> {
        loop {
            match self.git.fetch_through(&|tool, args| self.run(tool, args)) {
                Ok(tip) => {
                    self.reads.set(0);
                    return Ok(tip);
                }
                Err(why) => {
                    self.again(&self.reads, &format!("the read of {REMOTE}/{MAIN}"), &why)?;
                }
            }
        }
    }

    /// Where main stands for a push of `landing` onto `onto`, read afresh.
    fn observe(&self, landing: &str, onto: &str) -> Result<PushObservation, String> {
        let tip = self.main()?;
        if tip == landing || self.git.is_ancestor(landing, &tip)? {
            return Ok(PushObservation::Landed);
        }
        if tip != onto {
            return Ok(PushObservation::Moved(tip));
        }
        Ok(PushObservation::Unchanged)
    }

    /// Push `landing` to main, expecting main at `onto`: `None` once it landed, or main's new
    /// tip where it moved ([`Self::settle`]). A push that fails with no typed refusal (one this
    /// tool's hang detector ended included) has an unknown outcome and is never made again: main
    /// is read, and one that holds the landing took it; else what a later run must settle first
    /// is kept ([`Self::unsettled`]).
    fn push(
        &self,
        landing: &str,
        onto: &str,
        budget: &mut aterm_land::Budget,
    ) -> Result<Option<String>, String> {
        let run = |tool, args: &[std::ffi::OsString]| self.run(tool, args);
        let remote = crate::remote::Remote::open(self.git.root, REMOTE, &run)?;
        let write = |tool, args: &[std::ffi::OsString]| self.write(tool, args);
        let unknown = |why: String| {
            *self.unsettled.borrow_mut() = Some(format!(
                "the write of {landing} to {MAIN} ended with an unknown outcome; where agh \
                 journaled it, its intent stays open (`agh intent list --open`) until it is \
                 settled, or retracted once the write is known not to have landed"
            ));
            why
        };
        push_after_intents(
            budget,
            &|| {
                let pushed = remote
                    .push_with(
                        crate::remote::WriteRunner(&write),
                        &format!("refs/heads/{MAIN}"),
                        Some(onto),
                        landing,
                    )
                    .or_else(|why| match self.observe(landing, onto) {
                        Ok(PushObservation::Landed) => Ok(crate::remote::Pushed::Done),
                        Ok(PushObservation::Moved(_) | PushObservation::Unchanged) => {
                            Err(unknown(format!(
                                "{why}; {REMOTE}/{MAIN} does not hold the landing, and a push \
                                 whose outcome is unknown is not made again"
                            )))
                        }
                        Err(read) => Err(unknown(format!(
                            "{why}; whether {REMOTE}/{MAIN} holds the landing was not read: \
                             {read}"
                        ))),
                    })?;
                if !matches!(pushed, crate::remote::Pushed::Unsent(_)) {
                    self.unsent.set(0);
                }
                Ok(pushed)
            },
            &|intent| {
                (self.say)(&format!(
                    "verify land: waiting up to {}s for agh atomic intent {} to close; this push \
                     sent nothing",
                    WRITE.as_secs(),
                    intent.as_str()
                ));
                // Bound this wait without inferring whether the other owner is still alive
                // or whether its write was applied from the age of its intent alone.
                let until = Instant::now() + WRITE;
                self.wait_for_intent(intent, until)
            },
            // The refused attempt sent nothing. Where main is unchanged the same expected-ref
            // push runs again; this is not replay of an unknown write.
            &|| self.observe(landing, onto),
            &|why| self.settle(landing, onto, why),
            &|why| self.again(&self.unsent, "the push", why),
        )
    }

    fn wait_for_intent(&self, intent: &agh::IntentId, until: Instant) -> Result<(), String> {
        let remaining = || {
            if let Some(signal) = (self.interrupt)() {
                return Err(format!(
                    "the wait on another writer's atomic intent ended: the landing took \
                             signal {signal}"
                ));
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                *self.unsettled.borrow_mut() = Some(format!(
                    "agh intent {} stayed open for this wait's {}s budget: its outcome \
                     remains unresolved; retract it only once its write is known not \
                     to have landed (`agh intent list --open`)",
                    intent.as_str(),
                    WRITE.as_secs()
                ));
                return Err(format!(
                    "another writer's atomic intent stayed open for {}s",
                    WRITE.as_secs()
                ));
            }
            Ok(left)
        };
        let run = |tool, args: &[std::ffi::OsString]| {
            remaining()?;
            (self.step)(
                self.git.root,
                self.git.make,
                tool,
                args,
                until.min(Instant::now() + NETWORK),
                RemoteMode::Read,
            )
        };
        let remote = crate::remote::Remote::open(self.git.root, REMOTE, &run)?;
        remote.wait_for_intent(intent, &|| {
            (self.pause)(remaining()?.min(Duration::from_millis(100)));
            Ok(())
        })
    }

    /// Where a push of `landing` onto `onto` git reported failed (`why`) left main, fetched: a
    /// main that holds the landing took it (a connection lost after the server moved the ref
    /// loses only its report), `None`; one still at `onto` refused it, an error and never a
    /// retry; else main moved, its new tip.
    fn settle(&self, landing: &str, onto: &str, why: &str) -> Result<Option<String>, String> {
        match self.observe(landing, onto)? {
            PushObservation::Landed => Ok(None),
            PushObservation::Unchanged => Err(why.to_owned()),
            PushObservation::Moved(tip) => Ok(Some(tip)),
        }
    }
}

/// Whether the checked commit, judged over the cone since main commit `merged`, composes onto
/// main's tip by AGENTS.md's rule ([`Git::independent_changes`]).
#[derive(Clone, Debug)]
enum Composition {
    /// It does: the clean merge's tree and the rule's reason.
    Composes { tree: String, why: String },
    /// Its judgment stands for the tree it ran on, and a round ([`recheck`]) judges the merge:
    /// the clean merge's tree, and the shared paths ([`Git::shared`]).
    Rechecks { tree: String, shared: Vec<String> },
    /// The merge conflicts (the reason names where), and the judgment still stands for the
    /// tree it ran on: it finishes and is kept, the merge stops on the conflict for the person,
    /// and the merge they commit is judged as a round ([`Git::kept_merge`]).
    Held(String),
    /// It does not ([`Doom`]): a merge and a new judgment come instead.
    Doomed(Doom),
}

/// Why main's tip dooms a judgment ([`Composition::Doomed`]): the reason, the paths the tip
/// shares with the branch as far as they were read, and, where their reach decided it
/// ([`reaches_all`]), what main's change with those paths reaches and what the branch's own
/// change reaches. The landing decides by the reason alone; the rest is what its run records keep
/// of an abort ([`records`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Doom {
    pub(crate) why: String,
    pub(crate) shared: Vec<String>,
    pub(crate) reach: Option<(Reach, Reach)>,
}

/// [`Seams::reach`]'s question: the checkout's root, the commit the change is read since, and
/// the changed paths asked about, or none for the whole change since that commit.
pub type Reaches<'a> = dyn Fn(&Path, &str, Option<&[String]>) -> Reach + 'a;

/// The branch's reach over the cone since `merged`, read once per main commit (`cache`); a
/// reach that was not read is asked again for the next tip.
fn branch_reach(
    git: &Git<'_>,
    reach: &Reaches<'_>,
    cache: &std::cell::RefCell<Option<(String, Reach)>>,
    merged: &str,
) -> Reach {
    if let Some((base, found)) = &*cache.borrow()
        && base == merged
    {
        return found.clone();
    }
    let found = reach(git.root, merged, None);
    if !matches!(found, Reach::Unread(_)) {
        *cache.borrow_mut() = Some((merged.to_owned(), found.clone()));
    }
    found
}

/// Why nothing a judgment of the branch produces stands against a main tip whose change (with
/// the paths it shares with the branch) reaches `main`, where the branch's own reach is
/// `branch`: a round judges every crate of the branch's cone that `main` holds
/// (`crate::changed::resolve_round`), so it would judge all of them again where `main` is every
/// crate, or every crate the branch's change reaches, or where the branch's change reaches
/// every crate (a round of a widened cone is the whole workspace), or where `main` holds none
/// of the crates the branch's change reaches (a round that would leave nothing of a cone that
/// holds crates judges all of it: `crate::changed::meet`). `None` where a round judges less
/// than the judgment did, and where a reach was not read ([`Reach::Unread`]): nothing is then
/// known to be lost by letting the judgment finish.
fn reaches_all(main: &Reach, branch: &Reach) -> Option<String> {
    match (main, branch) {
        (Reach::Unread(_), _) | (_, Reach::Unread(_)) => None,
        (Reach::Everything(why), _) => Some(format!(
            "what it changed, with the paths it shares with the branch, reaches every crate \
             ({why})"
        )),
        (_, Reach::Everything(why)) => Some(format!(
            "the branch's change reaches every crate ({why}), so a round would judge all of it \
             again"
        )),
        (Reach::Crates(main), Reach::Crates(branch))
            if !branch.is_empty() && branch.is_subset(main) =>
        {
            Some(format!(
                "what it changed, with the paths it shares with the branch, reaches every crate \
                 the branch's change reaches ({})",
                branch.iter().cloned().collect::<Vec<_>>().join(", ")
            ))
        }
        (Reach::Crates(main), Reach::Crates(branch))
            if !branch.is_empty() && main.is_disjoint(branch) =>
        {
            Some(format!(
                "what it changed, with the paths it shares with the branch, reaches none of the \
                 crates the branch's change reaches ({}), so a round would judge all of them \
                 again",
                branch.iter().cloned().collect::<Vec<_>>().join(", ")
            ))
        }
        (Reach::Crates(_), Reach::Crates(_)) => None,
    }
}

/// The reach a round of main tip `tip` is bound to judge ([`Seams::reach`]): of `judged`, the
/// shared paths a crate may own and the files that may read shared Markdown
/// ([`Sharing::judged`]), with the paths of main's change since `merged` that a crate may own
/// by `layout`'s tree ([`Git::classed`]): a round judges main's change within the branch's
/// cone wherever it judges a crate (`crate::changed::resolve_round`). A path of main's that Git
/// names in no UTF-8 is read as Git would print it.
fn round_reach(
    git: &Git<'_>,
    reach: &Reaches<'_>,
    (merged, tip, layout): (&str, &str, &str),
    judged: &[String],
) -> Result<Reach, String> {
    let theirs: Vec<String> = git
        .files(merged, tip)?
        .iter()
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    let (_, theirs) = git.classed(&theirs, layout)?;
    let paths: BTreeSet<String> = judged.iter().cloned().chain(theirs).collect();
    let paths: Vec<String> = paths.into_iter().collect();
    Ok(reach(git.root, merged, Some(paths.as_slice())))
}

/// Test main's tip `tip` against `checked` (HEAD, merging `merged`): seconds of Git plumbing,
/// and, only where the two share a path a crate may own or Markdown a file may read, the reach
/// of what the round would judge and of the branch ([`round_reach`], [`Seams::reach`], the
/// branch's cached in `cache`); the same whether the judgment still runs or has passed.
///
/// A clean merge that is not independent ([`Independence::Blocked`]) is answered by a round
/// ([`Composition::Rechecks`]) unless nothing a judgment of `checked` produces can stand:
/// a shared path Git names in no UTF-8, shared no-reach Markdown the rule does not hold for
/// ([`Git::no_reach_holds`]: it is no regular file on every side, the merge holds another
/// no-reach rule, or a file no workspace member owns may read it, so no cone holds that
/// reader), or a round that would judge every crate the branch's change reaches
/// ([`reaches_all`]). Shared Markdown a member's file may read cuts nothing by itself: it
/// stands for those files ([`Sharing::judged`]), whose crates within the branch's cone the
/// round judges, with what main's change reaches there. A tip that does not descend from
/// `merged` is doomed as well. A merge that conflicts keeps the judgment
/// ([`Composition::Held`]) unless the same holds of the merge a person then makes
/// ([`conflicting`]).
fn composition(
    git: &Git<'_>,
    reach: &Reaches<'_>,
    cache: &std::cell::RefCell<Option<(String, Reach)>>,
    merged: &str,
    tip: &str,
    checked: &str,
) -> Result<Composition, String> {
    let compose = if git.is_ancestor(merged, tip)? {
        Some(git.merge_tree(tip, checked)?)
    } else {
        None
    };
    let mut shared = Vec::new();
    let mut reaches = None;
    let why = match &compose {
        None => format!("it does not descend from {merged}, the main the branch merged"),
        Some(Compose::Tree(tree)) => {
            let blocked = match git.independent_changes(merged, tip, checked, tree)? {
                Independence::Composes(why) => {
                    return Ok(Composition::Composes {
                        tree: tree.clone(),
                        why,
                    });
                }
                blocked @ Independence::Blocked { .. } => blocked,
            };
            let mut unnamed = None;
            for path in git.shared(merged, tip, checked, tree)? {
                match String::from_utf8(path) {
                    Ok(path) => shared.push(path),
                    Err(error) => {
                        unnamed.get_or_insert_with(|| display_git_path(error.as_bytes()));
                    }
                }
            }
            let whole = if let Some(path) = unnamed {
                Some(format!("{path} is no path a cone can be read from"))
            } else if shared.is_empty() {
                Some("no shared path was found to judge".to_owned())
            } else {
                match git.no_reach_holds(&shared, &[checked, tip, tree], tree)? {
                    Err(why) => Some(why),
                    Ok(found) => match found.unowned() {
                        Some(why) => Some(why),
                        // Markdown no file may read: the source readers alone answer for it.
                        None if found.owned.is_empty() && found.readers.is_empty() => None,
                        None => {
                            let main =
                                round_reach(git, reach, (merged, tip, tree), &found.judged())?;
                            let branch = branch_reach(git, reach, cache, merged);
                            let whole = reaches_all(&main, &branch).map(|why| found.counting(why));
                            reaches = Some((main, branch));
                            whole
                        }
                    },
                }
            };
            match whole {
                None => {
                    return Ok(Composition::Rechecks {
                        tree: git.landing_tree_of(tip, checked, tree)?,
                        shared,
                    });
                }
                Some(why) => format!("{blocked}; {why}"),
            }
        }
        Some(Compose::Conflict(conflicted)) => {
            return conflicting(git, reach, cache, merged, tip, checked, conflicted);
        }
    };
    Ok(Composition::Doomed(Doom {
        why,
        shared,
        reach: reaches,
    }))
}

/// Main's tip `tip`, whose merge into `checked` (merging `merged`) conflicts in `conflicted`:
/// the judgment of `checked` is kept through the conflict ([`Composition::Held`]) unless
/// nothing it produces can stand for the merge a person makes of the two, which
/// [`Git::kept_merge`] and [`recheck`] judge over the paths that merge shares. Those are not
/// known before it is made; what they hold at least is each path both sides changed to
/// different entries and each conflicted path. Doomed where that merge is bound to be judged
/// in full ([`round_delta`]): one of those paths is named in no UTF-8, can replan every crate
/// ([`crate::changed::REPLANNING_PATHS`]), is a submodule's entry on either side, or is
/// no-reach Markdown the rule does not hold for on the two sides ([`Git::no_reach_holds`],
/// by `checked`'s layout and code: it is no regular file in both), or that a file may read
/// ([`Sharing::read`]: a round over its readers answers only for the landing's own merge).
/// Doomed as well where the round of that merge would judge every crate the branch's change
/// reaches ([`round_reach`], [`reaches_all`]). Like the cut rule it decides only whether a judgment goes on, never
/// what a landing judges.
fn conflicting(
    git: &Git<'_>,
    reach: &Reaches<'_>,
    cache: &std::cell::RefCell<Option<(String, Reach)>>,
    merged: &str,
    tip: &str,
    checked: &str,
    conflicted: &[Vec<u8>],
) -> Result<Composition, String> {
    let why = format!(
        "the merge conflicts in {}",
        conflicted
            .iter()
            .map(|path| display_git_path(path))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let doomed = |what: String, shared: &[String]| {
        Ok(Composition::Doomed(Doom {
            why: format!("{why}; {what}"),
            shared: shared.to_vec(),
            reach: None,
        }))
    };
    let differing = git.files(checked, tip)?;
    let mut paths: BTreeSet<Vec<u8>> = git
        .files(merged, tip)?
        .intersection(&git.files(merged, checked)?)
        .filter(|path| differing.contains(*path))
        .cloned()
        .collect();
    paths.extend(conflicted.iter().cloned());
    let mut shared = Vec::new();
    for path in paths {
        match String::from_utf8(path) {
            Ok(path) => shared.push(path),
            Err(error) => {
                return doomed(
                    format!(
                        "{} is no path a cone can be read from",
                        display_git_path(error.as_bytes())
                    ),
                    &shared,
                );
            }
        }
    }
    if let Some(path) = shared
        .iter()
        .find(|path| crate::changed::replans_everything(path))
    {
        return doomed(
            format!(
                "the merge that resolves it shares {path}, which can change how every crate is \
                 built or linted, so it is judged in full"
            ),
            &shared,
        );
    }
    if let Some(path) = git.gitlink(&shared, &[checked, tip])? {
        return doomed(
            format!(
                "the merge that resolves it shares the submodule entry {path}, so it is judged \
                 in full"
            ),
            &shared,
        );
    }
    let unheld = match git.no_reach_holds(&shared, &[checked, tip], checked)? {
        Ok(found) => found.read().ok_or(found.owned),
        Err(unheld) => Ok(unheld),
    };
    let owned = match unheld {
        Ok(unheld) => {
            return doomed(
                format!("{unheld}, so the merge that resolves it is judged in full"),
                &shared,
            );
        }
        Err(owned) => owned,
    };
    // A merge made by hand is judged over what main's change reaches whatever it shares
    // (`crate::changed::resolve_round`), so the reach is asked for shared Markdown too.
    let main = round_reach(git, reach, (merged, tip, checked), &owned)?;
    let branch = branch_reach(git, reach, cache, merged);
    Ok(match reaches_all(&main, &branch) {
        Some(whole) => Composition::Doomed(Doom {
            why: format!("{why}; {whole}"),
            shared,
            reach: Some((main, branch)),
        }),
        None => Composition::Held(why),
    })
}

/// Judge `at` (HEAD) over the cone since merged main `base`, to its first red, and decide its
/// reds as the run ends ([`decide`]). While it runs, each poll the run asks for
/// reads main's tip ([`Git::fetch_unless`], the landing's own read, held to [`NETWORK`] and ended
/// once the poll is moot) and tests a new one ([`composition`], the branch's reach read once for
/// the run); the first that is doomed
/// interrupts the run, and nothing it wrote counts: it is stated as interrupted
/// ([`Stated::interrupted`]). A tip the last composed one already holds is a stale read, never
/// tested ([`aterm_land::Watch::poll`]). A poll that cannot read or test main changes nothing,
/// and says so once per run of one reason, unless it became moot meanwhile. It runs as the
/// checkout's own verifier ([`built`]), which must be this image: else [`Judging::Elsewhere`],
/// before anything ran.
/// [`Judging::Passed`] when it passed (its judgment added to `held.stated`), else the exit to
/// stop with or the tip that interrupted it.
fn judge(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    at: &str,
    base: &str,
    held: &mut Held<'_>,
) -> Result<Judging, String> {
    let tree = git.tree(at)?;
    let image = match built(git, seams, (at, base, &tree), held)? {
        Built::Here(image) => image,
        Built::Elsewhere(image) => return Ok(Judging::Elsewhere(image)),
        Built::Exit(code) => return Ok(Judging::Exit(code)),
    };
    let Held {
        stated,
        reach: cache,
        tally,
        ..
    } = held;
    (seams.say)(&format!(
        "verify land: judging {tree} over the cone since {base}; it stops at its first red"
    ));
    let started = (seams.records.now)();
    let mut watch = aterm_land::Watch::new(base);
    // What the doomed tip shares and reaches, for the abort's record.
    let doom = std::cell::RefCell::new(None);
    let ran = {
        let Seams {
            judge, say, reach, ..
        } = &mut *seams;
        let reach = *reach;
        // Each closure of a poll says its own line; a poll runs them one after another.
        let say = std::cell::RefCell::new(say);
        let mut poll = |moot: Moot<'_>| {
            watch.poll(
                moot,
                &mut |moot| git.fetch_unless(NETWORK, moot),
                &mut |tip, composed| git.is_ancestor(tip, composed),
                &mut |tip| {
                    let found = composition(git, reach, cache, base, tip, at)?;
                    if let Composition::Doomed(found) = &found {
                        *doom.borrow_mut() = Some((tip.to_owned(), found.clone()));
                    }
                    // A tip the judgment outlives is said when it is read, once: the person
                    // then knows of a conflict, or of the round to come, before the run ends.
                    let went_on = match &found {
                        Composition::Rechecks { shared, .. } => Some(format!(
                            "shares {} with the branch: the judgment goes on and stands for the \
                             tree it runs on, and a round judges the merge once it has passed",
                            shared.join(", ")
                        )),
                        Composition::Held(why) => Some(format!(
                            "does not merge cleanly ({why}): the judgment goes on and is kept, \
                             and the merge will stop on the conflict for a person to resolve"
                        )),
                        Composition::Composes { .. } | Composition::Doomed(_) => None,
                    };
                    if let Some(went_on) = went_on {
                        (say.borrow_mut())(&format!(
                            "verify land: {REMOTE}/{MAIN} moved to {tip}, which {went_on}"
                        ));
                    }
                    Ok(engine::composition(found))
                },
                &mut |why| (say.borrow_mut())(&format!("verify land: {REMOTE}/{MAIN} was not read or tested during the judgment ({why}); it goes on")),
            )
        };
        judge(git.root, &image.path, base, &mut poll)
    };
    let doomed = watch.into_doomed();
    // A run the landing's own interrupt ended is no verdict, whatever ended it.
    if stopped(seams) {
        return Ok(Judging::Exit(exit::COULD_NOT_RUN));
    }
    // A run that ended by itself is judged as it ended, whatever a poll answered as it did.
    let interrupted = match (ran, doomed) {
        (Ran::Ended(judged), _) => Err(judged),
        (Ran::Interrupted { secs, queued }, Some((tip, why))) => {
            let queued = if queued {
                " (it was still queued for the judgment turn, and had judged nothing)"
            } else {
                ""
            };
            (seams.say)(&format!(
                "verify land: INTERRUPTED the judgment of {tree}{queued}: {REMOTE}/{MAIN} moved \
                 to {tip}, which does not compose: {why}"
            ));
            Ok((tip, secs))
        }
        (Ran::Interrupted { .. } | Ran::Unended { .. }, None) => {
            return Err(format!(
                "the judgment of {tree} was interrupted, though no poll asked for it"
            ));
        }
        // What it started still runs, and may write the build directory: a judgment beside it
        // could stop on what that writes, so none follows.
        (Ran::Unended { left, .. }, Some((tip, why))) => {
            (seams.say)(&format!(
                "verify land: INTERRUPTED the judgment of {tree}: {REMOTE}/{MAIN} moved to {tip}, \
                 which does not compose: {why}"
            ));
            (seams.say)(&format!(
                "verify land: COULD NOT RUN — the interrupted judgment of {tree} left {} still \
                 running, which did not end within {}s of being named: {}; nothing was pushed, \
                 and nothing is judged beside what may still write the build directory. Run \
                 verify land again once {} gone",
                processes(left.len()),
                UNENDED_BOUND.as_secs(),
                unended_words(&left),
                if left.len() == 1 { "it is" } else { "they are" },
            ));
            return Ok(Judging::Exit(exit::COULD_NOT_RUN));
        }
    };
    let moved = git.moved_from(at)?;
    let records = &mut seams.records;
    match &interrupted {
        Ok((tip, secs)) => {
            let cause = match doom.into_inner() {
                Some((doomed, found)) if doomed == *tip => records::doomed("cut", tip, &found),
                _ => land_run::Cause::new("cut", tip, ""),
            };
            let step = land_run::Record {
                started,
                secs: *secs,
                head: at.to_owned(),
                base: base.to_owned(),
                tree: tree.clone(),
                result: "cut".to_owned(),
                cause: Some(cause.clone()),
                ..land_run::Record::default()
            };
            records.run_record(land_run::Kind::Abort, step, &mut *seams.say);
            records.tally.next = Some(land_run::Cause::new("cut", tip, &cause.why));
        }
        Err(judged) => {
            if let Some(why) = &moved {
                let cause = records.cause("first");
                let step =
                    records::step((at, &tree, base), started, judged, Err(why.as_str()), cause);
                records.run_record(land_run::Kind::Judgment, step, &mut *seams.say);
            }
        }
    }
    seams.records.tally.base = base.to_owned();
    if let Some(why) = moved {
        (seams.say)(&format!(
            "verify land: COULD NOT RUN — during the run, {why}; nothing was pushed"
        ));
        return Ok(Judging::Exit(exit::COULD_NOT_RUN));
    }
    let judged = match interrupted {
        Ok((tip, secs)) => {
            stated.interrupted.push(Interrupted {
                tree,
                base: base.to_owned(),
                tip: tip.clone(),
                secs,
            });
            return Ok(Judging::Interrupted(tip));
        }
        Err(judged) => judged,
    };
    records::record(seams, (at, &tree, base), started, &judged);
    let decision = decide(&judged);
    {
        let records = &mut seams.records;
        let cause = records.cause("first");
        let step = records::step((at, &tree, base), started, &judged, Ok(&decision), cause);
        records.run_record(land_run::Kind::Judgment, step, &mut *seams.say);
        records.tally.markdown = false;
    }
    if !decision.counted.is_empty() {
        (seams.say)(&format!(
            "verify land: FAIL — {} failed; nothing was pushed",
            decision.counted.join(", ")
        ));
    }
    say_stopped(seams, &decision);
    tally.last = Some(Last {
        round: false,
        tree: tree.clone(),
        base: base.to_owned(),
        judged: judged.clone(),
        decision: decision.clone(),
    });
    if !decision.not_run.is_empty() {
        (seams.say)(&format!(
            "verify land: COULD NOT RUN — {} did not run; nothing was pushed",
            decision.not_run.join(", ")
        ));
    }
    if !decision.counted.is_empty() {
        return Ok(Judging::Exit(exit::FAILED));
    }
    if !decision.not_run.is_empty() {
        return Ok(Judging::Exit(exit::COULD_NOT_RUN));
    }
    stated.checked.push(Checked {
        tree,
        rows: judged
            .checks
            .iter()
            .map(|row| Row {
                check: row.check.clone(),
                word: row.word.clone(),
            })
            .collect(),
        base: base.to_owned(),
        secs: judged.secs,
    });
    Ok(Judging::Passed)
}

/// Say what the run's first counted red stopped ([`Decision::stopped`]), where it stopped any.
fn say_stopped(seams: &mut Seams<'_>, decision: &Decision) {
    if !decision.stopped.is_empty() {
        (seams.say)(&format!(
            "verify land: STOPPED at the first red — {} did not run: a red counts, so the run \
             stopped there",
            decision.stopped.join(", "),
        ));
    }
}

/// What the round of `at` (a merge of main tip `tip` into a judged commit) judges: the judged
/// commit and the paths the merge shares between the two sides ([`Git::shared`]), read from the
/// commit and the receipt alone, so the image a round is handed to reads the same, and a merge
/// this landing did not make (one a person committed after a conflict, [`Git::kept_merge`])
/// is read as its own is.
///
/// No round answers ([`Rounded::Whole`]) where a round's crates, with the source readers, are
/// not known to cover what the merge changed for the judged commit:
/// - `at` is not a merge of `tip` into the commit whose tree the receipt's last judgment or
///   round stands for, over a main `tip` descends from;
/// - a shared path is named in no UTF-8;
/// - shared no-reach Markdown fails its rule ([`Git::no_reach_holds`]: it is no regular file on
///   every side, or the merge holds another no-reach rule), whoever made the merge;
/// - a file no workspace member owns may read shared no-reach Markdown ([`Sharing::unowned`]):
///   no cone holds that reader;
/// - the merge is not the landing's own merge of the two ([`Delta::mechanical`]) and shares
///   no-reach Markdown a file may read ([`Sharing::read`]): the round over its readers
///   ([`Delta::readers`]) answers for the Markdown that merge makes, each side's change in it,
///   not for one a person wrote;
/// - the merge is not the landing's own merge of the two and shares a path
///   that can replan every crate ([`crate::changed::REPLANNING_PATHS`]: a lock, a root
///   manifest, a toolchain or lint file) or a submodule's entry. A round compares such a path
///   with the judged commit's, so a main change to it that the merge dropped reads as no
///   change at all, though main's other files no longer agree with it.
///
/// # Errors
/// A Git read failed.
fn round_delta(git: &Git<'_>, at: &str, tip: &str, stated: &Stated) -> Result<Rounded, String> {
    let whole = |why: String| Ok(Rounded::Whole(why));
    let parents = git.parents(at)?;
    let [judged, main] = parents.as_slice() else {
        return whole(format!(
            "{at} is no merge of two commits, so it is not a merge of {tip} into a judged commit"
        ));
    };
    if main != tip {
        return whole(format!("{at} merges {main}, not the main tip {tip}"));
    }
    let (previous, stood) = match stated.rechecks_since_judged().last() {
        Some(round) => (round.onto.as_str(), round.tree.as_str()),
        None => match stated.checked.last() {
            Some(checked) => (checked.base.as_str(), checked.tree.as_str()),
            None => {
                return whole(format!(
                    "no judgment passed before the round of {at}, so nothing stands for its \
                     first parent"
                ));
            }
        },
    };
    if git.tree(judged)? != stood {
        return whole(format!(
            "the first parent of {at} is {judged}, whose tree is not {stood}, the tree the \
             last judgment stands for"
        ));
    }
    if !git.is_ancestor(previous, tip)? {
        return whole(format!(
            "{tip} does not descend from {previous}, the main the judged commit merged"
        ));
    }
    let tree = git.tree(at)?;
    let mechanical = git.landing_tree(tip, judged)?.tree() == Some(tree.as_str());
    let mut shared = Vec::new();
    for path in git.shared(previous, tip, judged, &tree)? {
        match String::from_utf8(path) {
            Ok(path) => shared.push(path),
            Err(error) => {
                return whole(format!(
                    "the shared path {} is no path a cone can be read from",
                    display_git_path(error.as_bytes())
                ));
            }
        }
    }
    let found = match git.no_reach_holds(&shared, &[judged, tip, &tree], &tree)? {
        Ok(found) => found,
        Err(why) => return whole(why),
    };
    if let Some(why) = found.unowned() {
        return whole(why);
    }
    // A round of Markdown alone: every shared path is in the class and no file may read it.
    let markdown = !shared.is_empty() && found.owned.is_empty() && found.readers.is_empty();
    if !mechanical {
        if let Some(why) = found.read() {
            return whole(format!(
                "{why}, and the merge was resolved or edited by hand"
            ));
        }
        if let Some(path) = shared
            .iter()
            .find(|path| crate::changed::replans_everything(path))
        {
            return whole(format!(
                "the merge was resolved or edited by hand and shares {path}, which can change \
                 how every crate is built or linted"
            ));
        }
        if let Some(path) = git.gitlink(&shared, &[judged, tip, at])? {
            return whole(format!(
                "the merge was resolved or edited by hand and shares the submodule entry {path}"
            ));
        }
    }
    Ok(Rounded::Round {
        delta: Delta {
            judged: judged.clone(),
            shared,
            readers: found.files(),
            mechanical,
        },
        markdown,
    })
}

/// THE ROUND: judge `at` (HEAD, a merge of main tip `tip` into the commit a judgment or an
/// earlier round passed) with the dev set since `tip`, its crates selected from the shared
/// paths alone ([`round_delta`], [`Seams::recheck`]), as the merge's own verifier ([`built`]),
/// which must be this image: else [`Rechecking::Elsewhere`], before anything ran. A merge no
/// round answers for ([`Rounded::Whole`]) is said and left to a full judgment
/// ([`Rechecking::Whole`]), before a verifier is built. Its reds are
/// decided as a judgment's ([`decide`]). It passes only when each of [`RECHECKS`] also ran and
/// said `ok` (the source readers answer for every path no cone holds), and the checkout is
/// still at `at`, clean: the round is then added to `held.stated`.
fn recheck(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    at: &str,
    tip: &str,
    held: &mut Held<'_>,
) -> Result<Rechecking, String> {
    let tree = git.tree(at)?;
    let (delta, markdown) =
        match round_delta(git, at, tip, &held.stated).map_err(|why| format!("{why}; {RERUN}"))? {
            Rounded::Round { delta, markdown } => (delta, markdown),
            Rounded::Whole(why) => {
                (seams.say)(&format!(
                    "verify land: no round answers for {tree}: {why}; it is judged in full"
                ));
                seams.records.tally.next = Some(land_run::Cause::new("no-round", tip, &why));
                return Ok(Rechecking::Whole);
            }
        };
    let image = match built(git, seams, (at, tip, &tree), held)? {
        Built::Here(image) => image,
        Built::Elsewhere(image) => return Ok(Rechecking::Elsewhere(image)),
        Built::Exit(code) => return Ok(Rechecking::Exit(code)),
    };
    let Held { stated, tally, .. } = held;
    (seams.say)(&format!(
        "verify land: judging {tree} since {tip} over the crates of the branch's cone that the \
         paths it shares with the branch ({}), or what {REMOTE}/{MAIN} changed since the judged \
         commit, reach, {} over the branch's whole change{}{}; it stops at its first red",
        delta.shared.join(", "),
        RECHECKS.join(", "),
        if delta.mechanical {
            ""
        } else {
            "; the merge was resolved or edited by hand, so no shared path's crate is judged \
             alone"
        },
        if delta.readers.is_empty() {
            String::new()
        } else {
            format!(
                "; {} may read the Markdown among those paths, so each one's crate is judged \
                 too where the branch's cone holds it, with what builds on it there",
                delta.readers.join(", ")
            )
        },
    ));
    let started = (seams.records.now)();
    let judged = (seams.recheck)(git.root, &image.path, tip, &delta);
    if stopped(seams) {
        return Ok(Rechecking::Exit(exit::COULD_NOT_RUN));
    }
    seams.records.tally.base = tip.to_owned();
    if let Some(why) = git.moved_from(at)? {
        let records = &mut seams.records;
        let cause = records.cause("handed").sharing(&delta.shared);
        let step = records::step((at, &tree, tip), started, &judged, Err(why.as_str()), cause);
        records.run_record(land_run::Kind::Round, step, &mut *seams.say);
        (seams.say)(&format!(
            "verify land: COULD NOT RUN — during the round, {why}; nothing was pushed"
        ));
        return Ok(Rechecking::Exit(exit::COULD_NOT_RUN));
    }
    records::record(seams, (at, &tree, tip), started, &judged);
    let mut decision = decide(&judged);
    for check in RECHECKS {
        match judged.checks.iter().find(|row| row.check == check) {
            Some(row) if row.word == "skip" => {
                decision.not_run.push(format!("{check} (skipped)"));
            }
            Some(_) => {}
            None => decision.not_run.push(format!("{check} (not in the run)")),
        }
    }
    {
        let records = &mut seams.records;
        let cause = records.cause("handed").sharing(&delta.shared);
        let step = records::step((at, &tree, tip), started, &judged, Ok(&decision), cause);
        records.run_record(land_run::Kind::Round, step, &mut *seams.say);
        records.tally.markdown = markdown;
    }
    tally.last = Some(Last {
        round: true,
        tree: tree.clone(),
        base: tip.to_owned(),
        judged: judged.clone(),
        decision: decision.clone(),
    });
    if !decision.counted.is_empty() {
        (seams.say)(&format!(
            "verify land: FAIL — the round of {tree}: {} failed; nothing was pushed",
            decision.counted.join(", ")
        ));
        say_stopped(seams, &decision);
        return Ok(Rechecking::Exit(exit::FAILED));
    }
    if !decision.not_run.is_empty() {
        (seams.say)(&format!(
            "verify land: COULD NOT RUN — the round of {tree}: {} did not run; nothing was \
             pushed",
            decision.not_run.join(", ")
        ));
        return Ok(Rechecking::Exit(exit::COULD_NOT_RUN));
    }
    stated.rechecked.push(Rechecked {
        tree,
        rows: judged
            .checks
            .iter()
            .map(|row| Row {
                check: row.check.clone(),
                word: row.word.clone(),
            })
            .collect(),
        base: tip.to_owned(),
        onto: tip.to_owned(),
        secs: judged.secs,
        after: stated.checked.len(),
    });
    Ok(Rechecking::Passed)
}

/// Say that merging `main` conflicts in `paths`, which the merge leaves for the person, and why
/// no rule resolved them (`refusals`, one per path, each on the one line): [`exit::CONFLICT`],
/// which a landing queue tells from a red.
fn conflicted(seams: &mut Seams<'_>, main: &str, paths: &[Vec<u8>], refusals: &[String]) -> i32 {
    (seams.say)(&format!(
        "verify land: CONFLICT — merging {main} conflicts in {} ({}); no path was resolved and \
         nothing was pushed. Fix: resolve them, commit the merge and run it again",
        paths
            .iter()
            .map(|path| display_git_path(path))
            .collect::<Vec<_>>()
            .join(", "),
        refusals
            .iter()
            .map(|why| why.replace(['\n', '\r'], " "))
            .collect::<Vec<_>>()
            .join("; ")
    ));
    exit::CONFLICT
}

/// The landed line. Its re-judges count each merge of a moved main judged again, the ones an
/// interrupted judgment led to included, as the receipt does ([`Stated::rejudges`]).
fn landed_line(landing: &str, stated: &Stated) -> String {
    let last = stated.checked.last().map_or("", |c| c.tree.as_str());
    let rejudges = stated.rejudges();
    let interrupted = stated.interrupted.len();
    let mut line = format!(
        "verify land: LANDED {landing} on {REMOTE}/{MAIN}: checked {last} ({rejudges} re-judge{}",
        if rejudges == 1 { "" } else { "s" }
    );
    if interrupted > 0 {
        line.push_str(&format!(", {interrupted} after an interrupted judgment"));
    }
    line.push(')');
    for recheck in &stated.rechecked {
        line.push_str(&format!(", rechecked onto {}", recheck.onto));
    }
    if let Some(onto) = &stated.composed_onto {
        line.push_str(&format!(", composed onto {onto}"));
    }
    line
}

/// How a merge of main into the checkout went.
enum Merge {
    /// Merged and committed: `resolved` names each rule that ran and its path, empty where git
    /// merged cleanly.
    Clean { resolved: Vec<String> },
    /// Stopped on conflicts, left for the person: the conflicted paths, and why no rule resolved
    /// them, one line per path.
    Conflict {
        paths: Vec<Vec<u8>>,
        refusals: Vec<String>,
    },
}

/// Render Git path bytes on one diagnostic line, preserving invalid bytes as escapes.
fn display_git_path(path: &[u8]) -> String {
    let shown = match std::str::from_utf8(path) {
        Ok(path) => path.escape_debug().to_string(),
        Err(_) => path
            .iter()
            .copied()
            .flat_map(std::ascii::escape_default)
            .map(char::from)
            .collect(),
    };
    format!("\"{shown}\"")
}

/// Validate a historical composition with the same file rule as the landing writer.
/// A re-judgment's `base` can be an earlier checked branch commit; composition instead
/// compares the two parents from their common main ancestor.
pub(crate) fn receipt_composition(
    root: &Path,
    path: &std::ffi::OsStr,
    checked: &str,
    main: &str,
    tree: &str,
) -> Result<bool, String> {
    let make = |root: &Path| {
        let mut command = crate::git_provider::command();
        command
            .current_dir(root)
            .env("PATH", path)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALLOW_PROTOCOL", "");
        for key in crate::changed::GIT_REDIRECTS
            .into_iter()
            .chain(PATHSPEC_MODES)
        {
            command.env_remove(key);
        }
        command
    };
    let git = Git { root, make: &make };
    let base = git.local(&["merge-base", "--all", checked, main])?;
    if base.split_whitespace().count() != 1 {
        return Ok(false);
    }
    let base = base.trim();
    if git.merge_tree(main, checked)?.tree() != Some(tree) {
        return Ok(false);
    }
    Ok(matches!(
        git.independent_changes(base, main, checked, tree)?,
        Independence::Composes(_)
    ))
}

/// Whether a clean merge of a moved main is independent of the branch's changes by AGENTS.md's
/// rule, so the branch's pass composes onto it without another judgment ([`composition`]).
#[derive(Debug, PartialEq, Eq)]
enum Independence {
    /// It composes: the writer's report of why.
    Composes(String),
    /// It does not: the shared paths still blocking it once exactly convergent entries were
    /// discarded, and, where the endpoint entries could not be read, why (no path was then
    /// discarded, so `paths` is the whole overlap). `lock` is set exactly where `paths` is
    /// `Cargo.lock` alone: why its isolated product-version exception did not compose it, a
    /// difference the read trees decided or the reason it could not be judged.
    Blocked {
        paths: Vec<Vec<u8>>,
        unread: Option<String>,
        lock: Option<version::Refusal>,
    },
}

/// A composition's reason, or a refusal's words after "does not compose: ".
impl std::fmt::Display for Independence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Composes(why) => f.write_str(why),
            Self::Blocked {
                paths,
                unread,
                lock,
            } => {
                let named = paths
                    .iter()
                    .map(|path| display_git_path(path))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "its new commits touch the branch's {named}")?;
                match unread {
                    None => f.write_str(
                        ", where the checked tree's entry differs from new main's or the merge's",
                    )?,
                    Some(why) => write!(
                        f,
                        " (the endpoint entries could not be read, so no shared path was \
                         discarded as convergent: {why})"
                    )?,
                }
                match lock {
                    None => Ok(()),
                    Some(version::Refusal::Differs(why)) => write!(
                        f,
                        ", and its change is not the isolated product-version move that alone \
                         may compose: {why}"
                    ),
                    Some(version::Refusal::Unjudged(why)) => write!(
                        f,
                        ", and whether its change is the isolated product-version move that \
                         alone may compose could not be judged: {why}"
                    ),
                }
            }
        }
    }
}

/// Whether the shared paths still blocking a composition are `Cargo.lock` alone: the one overlap
/// the isolated product-version exception may compose ([`version::independent_lock`]).
fn is_lock_alone(paths: &[Vec<u8>]) -> bool {
    matches!(paths, [path] if path.as_slice() == b"Cargo.lock")
}

/// What composing a checked commit onto moving main makes ([`Git::merge_tree`]): its tree, or
/// a conflict, after which an ordinary worktree merge ([`Git::merge`]) is resolved by rule or
/// stops for the person.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Compose {
    /// The clean merge's tree.
    Tree(String),
    /// Git's merge conflicts: the conflicted paths, each once, as Git names them.
    Conflict(Vec<Vec<u8>>),
}

impl Compose {
    /// The clean merge's tree, where there is one.
    fn tree(&self) -> Option<&str> {
        match self {
            Compose::Tree(tree) => Some(tree),
            Compose::Conflict(_) => None,
        }
    }
}

/// How [`Git::update_submodules_through`] runs one remote step of a pin import in the
/// submodule repository at the path, held to the instant: [`crate::remote::run`] in production.
type SubmoduleRun<'a> = dyn Fn(
        &Path,
        crate::remote::Tool,
        &[std::ffi::OsString],
        Instant,
    ) -> Result<std::process::Output, String>
    + 'a;

/// What the Markdown rule found of a merge's shared paths ([`Git::no_reach_holds`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct Sharing {
    /// The shared paths outside the no-reach class, which a crate may own.
    owned: Vec<String>,
    /// Each file of the merge that may read a shared path of the class ([`Git::readers`]), with
    /// the first such path it may read and whether a workspace member owns the file.
    readers: BTreeMap<String, (String, bool)>,
}

impl Sharing {
    /// Why a file may read shared Markdown, of the first such file; `None` where none may.
    fn read(&self) -> Option<String> {
        self.readers.iter().next().map(|(file, (path, _))| {
            format!("{path} is named in {file}'s code, so a build or test may read it")
        })
    }

    /// Why no round answers for shared Markdown a file may read: the first such file that no
    /// workspace member owns (a fork's, a nested or excluded package's, the root package's, or
    /// no package's), whose reads no crate's rows are known to hold. `None` where a member
    /// owns each reader.
    fn unowned(&self) -> Option<String> {
        self.readers
            .iter()
            .find(|(_, (_, member))| !member)
            .map(|(file, (path, _))| {
                format!(
                    "{path} is named in {file}'s code, so a build or test may read it, and no \
                     workspace member owns that file, so no crate's rows answer for what it \
                     reads"
                )
            })
    }

    /// The files that may read shared Markdown, in byte order.
    fn files(&self) -> Vec<String> {
        self.readers.keys().cloned().collect()
    }

    /// The paths whose crates a round judges for what the merge shares
    /// (`crate::changed::resolve_round`): each shared path a crate may own, and, for shared
    /// Markdown, each file that may read it.
    fn judged(&self) -> Vec<String> {
        let paths: BTreeSet<String> = self.owned.iter().cloned().chain(self.files()).collect();
        paths.into_iter().collect()
    }

    /// `why` a round would judge every crate the branch's change reaches ([`reaches_all`]),
    /// naming the files that stand for shared Markdown where the reach was asked of any.
    fn counting(&self, why: String) -> String {
        if self.readers.is_empty() {
            return why;
        }
        format!(
            "{why}; the shared Markdown stands for the files that may read it ({})",
            self.files().join(", ")
        )
    }
}

/// A tree's layout as the land reads it ([`Git::laid`]).
struct Laid {
    /// What the no-reach class reads of the tree.
    layout: aterm_census::no_reach::Layout,
    /// Each folder that holds a `Cargo.toml`, the root apart.
    manifests: BTreeSet<String>,
}

impl Laid {
    /// Whether `path` is in the no-reach class over this layout.
    fn class(&self, path: &str) -> bool {
        aterm_census::no_reach::class(path, &self.layout)
    }

    /// Whether a workspace member owns `file`: the nearest folder above it that holds a
    /// manifest is a member's. The class answers that: it holds `<dir>/SPECS.md` exactly where
    /// `<dir>` holds a manifest and is a workspace member outside `docs/`. A file of the root
    /// package, of a package under `docs/`, of a fork, of a nested or excluded package or of no
    /// package is no member's here.
    fn member_owns(&self, file: &str) -> bool {
        let mut dir = file;
        while let Some((parent, _)) = dir.rsplit_once('/') {
            if self.manifests.contains(parent) {
                return self.class(&format!("{parent}/SPECS.md"));
            }
            dir = parent;
        }
        false
    }
}

/// Whether no-reach `path` is a member's spec table (`<dir>/SPECS.md` outside `docs/`), as
/// [`aterm_census::no_reach::may_read`] reads one.
fn spec_table(path: &str) -> bool {
    path.ends_with("/SPECS.md") && !path.starts_with("docs/")
}

/// Whether `file`, whose text is `text`, may walk every member's spec table: a string of its
/// code (in a Rust file, its doctests too; in a crate's Markdown, its Rust blocks) ends in
/// `SPECS.md`, whatever builds the rest of the path (`format!("{root}/crates/{name}/SPECS.md")`,
/// a `"/SPECS.md"` joined to a crate's folder), or a string or the text cannot be read. The
/// search [`aterm_census::no_reach::may_read`] counts only a bare name, or one placeholder
/// before it, as such a walk: a round over the readers judges no crate a reader it misses
/// belongs to, so the land counts every string that can end such a path.
fn walks_spec_tables(file: &str, text: &str) -> bool {
    let ends = |text: &str| match aterm_census::items::Source::read(text.to_owned()) {
        None => true,
        Some(source) => source
            .strings()
            .any(|(value, _)| value.is_none_or(|value| value.ends_with("SPECS.md"))),
    };
    if file.ends_with(".rs") {
        ends(text) || ends(&aterm_census::no_reach::doctests(text))
    } else {
        ends(&aterm_census::no_reach::fenced(text))
    }
}

/// Whether `file` is one of this crate's own landing or cone rule sources, or their tests:
/// `crates/aterm-verify/src/land*.rs` and `changed*.rs`. They spell a spec table's name and
/// the tracker documents' paths to choose, judge and resolve them, and in fixture trees, and
/// open none of the checkout's: the search for readers passes them over, as
/// [`aterm_census::no_reach::searched`] passes census's own. Counted, each made this crate a
/// reader of every member's spec table and of `docs/PLAN.md` and `docs/STATUS.md`, and a
/// landing that shared one of those with main was judged again for it. Every other file of
/// this crate is searched like any member's.
fn rule_source(file: &str) -> bool {
    file.strip_prefix("crates/aterm-verify/src/")
        .is_some_and(|name| name.starts_with("land") || name.starts_with("changed"))
}

/// Whether `file` of a tree, whose text is `text`, counts as a reader of no-reach `path`
/// ([`Git::readers`]): [`aterm_census::no_reach::may_read`] says it may read it, or `path` is a
/// member's spec table and the file may walk every member's ([`walks_spec_tables`]); never a
/// file the search does not read ([`aterm_census::no_reach::searched`]) or one of this
/// crate's rule sources ([`rule_source`]).
fn reads(file: &str, text: &str, path: &str) -> bool {
    aterm_census::no_reach::searched(file)
        && !rule_source(file)
        && (aterm_census::no_reach::may_read(file, text, path)
            || (spec_table(path) && walks_spec_tables(file, text)))
}

/// The checkout's git.
struct Git<'a> {
    root: &'a Path,
    make: &'a dyn Fn(&Path) -> Command,
}

/// A status-only Git result. Tailed stdout cannot supply semantic data through this type.
struct GitStatus {
    status: std::process::ExitStatus,
    stderr: Vec<u8>,
}

impl Git<'_> {
    /// Whether the checkout can hold a file at repository path `path` with no write outside it,
    /// reading only: refused where a part is empty, `.` or `..`, or where one of its existing
    /// parts (itself included) is a symbolic link or cannot be read.
    fn writable(&self, path: &str) -> Result<(), String> {
        let mut at = self.root.to_path_buf();
        for part in path.split('/') {
            if matches!(part, "" | "." | "..") {
                return Err(format!("{path:?} is not a plain repository path"));
            }
            at.push(part);
            match std::fs::symlink_metadata(&at) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(format!("{} is a symbolic link", at.display()));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(format!("{}: {error}", at.display())),
            }
        }
        Ok(())
    }

    /// The writer and historical reader share exact entry convergence and the isolated
    /// product-version exception. A composition's reason is the writer's report; a refusal
    /// names what still blocked it.
    fn independent_changes(
        &self,
        base: &str,
        main: &str,
        checked: &str,
        tree: &str,
    ) -> Result<Independence, String> {
        let theirs = self.files(base, main)?;
        let ours = self.files(base, checked)?;
        let mut overlap: Vec<Vec<u8>> = theirs.intersection(&ours).cloned().collect();
        if overlap.is_empty() {
            return Ok(Independence::Composes(
                "the merge is clean and its new commits touch none of the branch's files".into(),
            ));
        }
        // Optional endpoint evidence: discard a shared path only when its full
        // entry agrees in checked, main and the already verified clean merge.
        // Including tree entries distinguishes a deleted path from a directory.
        let unread = match (
            self.entry_changes(checked, main),
            self.entry_changes(checked, tree),
        ) {
            (Ok(main_changes), Ok(merge_changes)) => {
                overlap.retain(|path| main_changes.contains(path) || merge_changes.contains(path));
                if overlap.is_empty() {
                    return Ok(Independence::Composes(
                        "the merge is clean and every shared path has the same entry in the checked tree, new main and merge".into(),
                    ));
                }
                None
            }
            (Err(why), _) | (_, Err(why)) => Some(why),
        };
        let lock = if is_lock_alone(&overlap) {
            match version::independent_lock(self, base, main, checked, tree) {
                Ok((old, new)) => {
                    return Ok(Independence::Composes(format!(
                        "the merge is clean and its only differing shared file is Cargo.lock, where the isolated product version moved from {old} to {new}; every other lock byte is preserved"
                    )));
                }
                Err(refusal) => Some(refusal),
            }
        } else {
            None
        };
        Ok(Independence::Blocked {
            paths: overlap,
            unread,
            lock,
        })
    }
    /// THE SHARED PATHS of a merge `tree` of main commit `main` into checked commit `checked`,
    /// both descending from `base`: each path of the branch's change that the merge holds
    /// unlike the branch, each path of main's change that it holds unlike main, and each path
    /// it holds unlike both. Every other path the merge holds as one side alone left it. For a
    /// merge Git made cleanly these are the paths both sides changed, less those whose entry is
    /// the same in all three, and the paths the merge itself moved or made (a rename it
    /// followed).
    fn shared(
        &self,
        base: &str,
        main: &str,
        checked: &str,
        tree: &str,
    ) -> Result<BTreeSet<Vec<u8>>, String> {
        let from_checked = self.files(checked, tree)?;
        let from_main = self.files(main, tree)?;
        let mut shared: BTreeSet<Vec<u8>> = self
            .files(base, checked)?
            .intersection(&from_checked)
            .cloned()
            .collect();
        shared.extend(self.files(base, main)?.intersection(&from_main).cloned());
        shared.extend(from_checked.intersection(&from_main).cloned());
        Ok(shared)
    }

    /// THE MARKDOWN RULE (`docs/DESIGN-landing-economy.md` §2.1) over `paths`, the shared paths
    /// ([`Self::shared`]) of a merge `tree` of main's tip into a checked commit, `sides` being
    /// those two commits and the merge (before a merge that conflicts is made, the two commits,
    /// with the checked one as `tree`). A path in the no-reach class over `tree`'s own layout
    /// ([`aterm_census::no_reach::class`]) is in no crate's cone. Markdown changes a verdict
    /// only through a file that reads it, so the source readers, with the rows of each file
    /// that may read it, answer for it, which holds only where each such path is a regular
    /// non-executable file (mode 100644) in each of `sides`, so a rename on either side, a
    /// symlink, a gitlink or a deletion is judged in full; and `tree` holds this land's own
    /// no-reach rule ([`Self::holds_this_rule`]), so the class this binary read is the one
    /// census's guard holds the merge to.
    ///
    /// `Ok(found)` names the paths outside the class, which a crate may own (a shared
    /// `Cargo.lock` is one), and each file of `tree` that may read a path in it
    /// ([`Self::readers`]: [`aterm_census::no_reach::may_read`], a Rust file, or a crate's
    /// Markdown, outside `crates/aterm-census` naming its file name, a docs directory above it
    /// or, for a spec table, `SPECS.md` from its own crate, in code or a doctest: the census
    /// guard's backstop for a read it cannot resolve), with whether a workspace member owns
    /// that file ([`Laid::member_owns`]). `Err(why)` says which part does not hold for a path
    /// in the class.
    fn no_reach_holds(
        &self,
        paths: &[String],
        sides: &[&str],
        tree: &str,
    ) -> Result<Result<Sharing, String>, String> {
        let laid = self.laid(tree)?;
        let (paths, owned): (Vec<String>, Vec<String>) =
            paths.iter().cloned().partition(|path| laid.class(path));
        let mut found = Sharing {
            owned,
            readers: BTreeMap::new(),
        };
        if paths.is_empty() {
            return Ok(Ok(found));
        }
        for commit in sides.iter().copied() {
            let mut args = vec![
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "--full-tree",
                commit,
                "--",
            ];
            args.extend(paths.iter().map(String::as_str));
            let out = self.complete(&args, LOCAL)?;
            if !out.status.success() {
                return Err(format!(
                    "git ls-tree {commit}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let mut regular = BTreeSet::new();
            for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
                let entry = String::from_utf8_lossy(entry);
                if let Some((meta, path)) = entry.split_once('\t')
                    && meta.starts_with("100644 blob ")
                {
                    regular.insert(path.to_owned());
                }
            }
            if let Some(path) = paths.iter().find(|path| !regular.contains(path.as_str())) {
                return Ok(Err(format!(
                    "{path} is not one regular file in {commit}, one of the branch's and main's \
                     tips and their merge"
                )));
            }
        }
        if !self.holds_this_rule(tree)? {
            return Ok(Err(format!(
                "the merge's {RULE_PATH} is not the rule this land chose the shared paths by"
            )));
        }
        for path in &paths {
            for file in self.readers(tree, path)? {
                let member = laid.member_owns(&file);
                found
                    .readers
                    .entry(file)
                    .or_insert_with(|| (path.clone(), member));
            }
        }
        Ok(Ok(found))
    }

    /// `tree`'s layout: what the no-reach class reads of it, and each folder that holds a
    /// `Cargo.toml`.
    fn laid(&self, tree: &str) -> Result<Laid, String> {
        let listing = self.paths(&["ls-tree", "-r", "-z", "--name-only", "--full-tree", tree])?;
        let manifest = self
            .local(&["cat-file", "blob", &format!("{tree}:Cargo.toml")])
            .ok();
        let listed = || {
            listing
                .iter()
                .filter_map(|path| std::str::from_utf8(path).ok())
        };
        Ok(Laid {
            layout: aterm_census::no_reach::Layout::new(manifest.as_deref(), listed()),
            manifests: listed()
                .filter_map(|path| path.strip_suffix("/Cargo.toml"))
                .map(str::to_owned)
                .collect(),
        })
    }

    /// `paths` split by the no-reach class over `tree`'s own layout
    /// ([`aterm_census::no_reach::class`]): those in it, which no crate's cone holds, and those
    /// outside it, which a crate may own.
    fn classed(&self, paths: &[String], tree: &str) -> Result<(Vec<String>, Vec<String>), String> {
        let laid = self.laid(tree)?;
        Ok(paths.iter().cloned().partition(|path| laid.class(path)))
    }

    /// The first of `paths` that is a submodule's entry (a gitlink) in one of `commits`.
    fn gitlink(&self, paths: &[String], commits: &[&str]) -> Result<Option<String>, String> {
        if paths.is_empty() {
            return Ok(None);
        }
        for commit in commits {
            let mut args = vec![
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "--full-tree",
                commit,
                "--",
            ];
            args.extend(paths.iter().map(String::as_str));
            let out = self.complete(&args, LOCAL)?;
            if !out.status.success() {
                return Err(format!(
                    "git ls-tree {commit}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
                let entry = String::from_utf8_lossy(entry);
                if let Some((meta, path)) = entry.split_once('\t')
                    && meta.starts_with("160000 ")
                {
                    return Ok(Some(path.to_owned()));
                }
            }
        }
        Ok(None)
    }

    /// Every file of `tree` that may read no-reach `path` ([`aterm_census::no_reach::may_read`]:
    /// a Rust file, or a crate's Markdown, naming one of the path's
    /// [`aterm_census::no_reach::needles`] in code; for a member's spec table also each such
    /// file that may walk every member's, [`walks_spec_tables`]), in Git's order ([`reads`]).
    /// Each is a file of this repository's own tree: a submodule's sources are not searched,
    /// nor census's own ([`aterm_census::no_reach::searched`]), nor this crate's rule sources
    /// ([`rule_source`]).
    fn readers(&self, tree: &str, path: &str) -> Result<Vec<String>, String> {
        // No pathspec: a person's GIT_LITERAL_PATHSPECS or GIT_NOGLOB_PATHSPECS would make one
        // such as `*.rs` match nothing, and an empty search name no reader. The file names are
        // filtered here instead. A submodule's commits need not be in this repository's objects
        // (a checkout with `submodule.recurse` set would read them and fail): its sources are its
        // own.
        let mut args = vec![
            "-c",
            "submodule.recurse=false",
            "grep",
            "-z",
            "-l",
            "-I",
            "-F",
            "--no-color",
        ];
        for needle in aterm_census::no_reach::needles(path) {
            args.extend(["-e", needle]);
        }
        args.push(tree);
        let out = self.complete(&args, LOCAL)?;
        match out.status.code() {
            Some(0) => {}
            Some(1) => return Ok(Vec::new()),
            _ => {
                return Err(format!(
                    "git grep {path} {tree}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        let prefix = format!("{tree}:");
        let mut readers = Vec::new();
        for found in out.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let found = String::from_utf8_lossy(found);
            let file = found.strip_prefix(&prefix).unwrap_or(&found);
            if !aterm_census::no_reach::searched(file) || rule_source(file) {
                continue;
            }
            let text = self.complete(&["cat-file", "blob", &format!("{tree}:{file}")], LOCAL)?;
            if !text.status.success() {
                return Err(format!(
                    "git cat-file {tree}:{file}: {}",
                    String::from_utf8_lossy(&text.stderr).trim()
                ));
            }
            let text = String::from_utf8_lossy(&text.stdout);
            if reads(file, &text, path) {
                readers.push(file.to_owned());
            }
        }
        Ok(readers)
    }

    /// Whether `tree` holds this land's no-reach rule ([`aterm_census::no_reach::SOURCE`]) at its
    /// path: the class this binary chose the shared paths by is then the one census's guard holds
    /// the merge to.
    fn holds_this_rule(&self, tree: &str) -> Result<bool, String> {
        let out = self.complete(&["cat-file", "-t", &format!("{tree}:{RULE_PATH}")], LOCAL)?;
        if !out.status.success() {
            return Ok(false);
        }
        let out = self.complete(&["cat-file", "blob", &format!("{tree}:{RULE_PATH}")], LOCAL)?;
        if !out.status.success() {
            return Err(format!(
                "git cat-file {tree}:{RULE_PATH}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out.stdout == aterm_census::no_reach::SOURCE.as_bytes())
    }

    /// `git <args>` run to its end for its status alone, ended only once it stalled for
    /// `stalled` ([`STEP`]).
    fn status(&self, args: &[&str], stalled: Duration) -> Result<GitStatus, String> {
        let mut cmd = (self.make)(self.root);
        cmd.args(args);
        let out = aterm_uds::deadline::output_while_process_progressing(&mut cmd, stalled, STEP)
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        Ok(GitStatus {
            status: out.status,
            stderr: out.stderr,
        })
    }

    /// `git <args>` run to its end with its whole stdout. It takes as long as the machine makes
    /// it: it is ended only once it printed nothing and its process tree used no CPU and did no
    /// I/O for `stalled`, which is a hang and reads as one.
    fn complete(&self, args: &[&str], stalled: Duration) -> Result<std::process::Output, String> {
        let mut cmd = (self.make)(self.root);
        cmd.args(args);
        aterm_uds::deadline::output_while_process_progressing_complete_stdout_with_limit(
            &mut cmd,
            stalled,
            STEP,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|error| format!("git {}: {error}", args.join(" ")))
    }

    fn ok(&self, args: &[&str], stalled: Duration) -> Result<String, String> {
        let out = self.complete(args, stalled)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout)
                .trim_end_matches(['\r', '\n'])
                .to_owned())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn local(&self, args: &[&str]) -> Result<String, String> {
        self.ok(args, LOCAL)
    }

    fn head(&self) -> Result<String, String> {
        self.local(&["rev-parse", "--verify", "HEAD^{commit}"])
    }

    /// Where the landing writes its handoff ([`handoff::FILE`]): the checkout's own git
    /// directory, absolute.
    fn handoff_file(&self) -> Result<PathBuf, String> {
        Ok(PathBuf::from(self.local(&["rev-parse", "--absolute-git-dir"])?).join(handoff::FILE))
    }

    /// Take the checkout's landing lock ([`LOCK_FILE`], in its own git directory), held until
    /// the guard drops or the process ends or execs (the descriptor is close-on-exec, so a
    /// handoff's receiver takes it again): else why not, a landing that holds it named first.
    fn hold(&self) -> Result<aterm_toolchain::record_lock::RecordLock, String> {
        let file = PathBuf::from(self.local(&["rev-parse", "--absolute-git-dir"])?).join(LOCK_FILE);
        let open = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&file)
            .map_err(|error| format!("{} could not be opened: {error}", file.display()))?;
        aterm_toolchain::record_lock::RecordLock::try_lock(open, 0).map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => format!(
                "another landing runs in {}: it holds {} until it ends (`aterm ctl board` names \
                 the worker a `verify land {}` of this session started), and nothing was landed \
                 here. Fix: wait for it to end, then run this again",
                self.root.display(),
                file.display(),
                tab::FLAG
            ),
            std::fs::TryLockError::Error(error) => {
                format!("{} could not be locked: {error}", file.display())
            }
        })
    }

    /// The checkout's changes no commit holds (`status --porcelain`), submodules included.
    fn changes(&self) -> Result<String, String> {
        self.local(&[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ])
    }

    /// How the checkout left `at`, committed and clean: `None` where it still is, else why not.
    fn moved_from(&self, at: &str) -> Result<Option<String>, String> {
        let head = self.head()?;
        if head != at {
            return Ok(Some(format!("HEAD moved from {at} to {head}")));
        }
        let changes = self.changes()?;
        Ok((!changes.is_empty())
            .then(|| format!("the checkout gained changes no commit holds:\n{changes}\n")))
    }

    fn tree(&self, commit: &str) -> Result<String, String> {
        self.local(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])
    }

    fn parents(&self, commit: &str) -> Result<Vec<String>, String> {
        let line = self.local(&["rev-list", "--parents", "-n", "1", commit])?;
        Ok(line.split_whitespace().skip(1).map(str::to_owned).collect())
    }

    fn admit_push_source(&self) -> Result<(), String> {
        let until = Instant::now() + LOCAL;
        let run = |tool, args: &[std::ffi::OsString]| {
            crate::remote::run_unless(self.root, self.make, tool, args, until, &mut taken)
        };
        crate::remote::Remote::open(self.root, REMOTE, &run)?.admit_push_source()
    }

    /// Main's tip, fetched into its remote-tracking ref (which the harness's probe reads): its
    /// remote steps held to [`NETWORK`] from now, the ref update to [`LOCAL`].
    fn fetch(&self) -> Result<String, String> {
        let until = Instant::now() + NETWORK;
        self.fetch_through(&|tool, args| {
            crate::remote::run_unless(self.root, self.make, tool, args, until, &mut taken)
        })
    }

    /// [`Self::fetch`], its remote steps held to `within` from now and ended once `stopped`
    /// reads `true` (read at least every [`aterm_uds::deadline::STOP_TICK`]), which ends the
    /// read with an error; the ref update keeps [`LOCAL`].
    fn fetch_unless(
        &self,
        within: Duration,
        stopped: &mut dyn FnMut() -> bool,
    ) -> Result<String, String> {
        let until = Instant::now() + within;
        let stopped = std::cell::RefCell::new(stopped);
        self.fetch_through(&|tool, args| {
            let mut stopped = stopped.borrow_mut();
            crate::remote::run_unless(self.root, self.make, tool, args, until, &mut **stopped)
        })
    }

    /// Main's tip read through `run`, then cached without overwriting another writer's
    /// observation made during that read. An absent tracking ref is created conditionally.
    fn fetch_through(&self, run: &crate::remote::Runner<'_>) -> Result<String, String> {
        let reference = format!("refs/remotes/{REMOTE}/{MAIN}");
        let before = self.complete(&["rev-parse", "--quiet", "--verify", &reference], LOCAL)?;
        let expected = if before.status.success() {
            String::from_utf8(before.stdout)
                .map_err(|_| "main's tracking ref is not UTF-8")?
                .trim()
                .to_owned()
        } else if before.status.code() == Some(1) && before.stdout.is_empty() {
            String::new()
        } else {
            return Err(format!(
                "cannot read main's tracking ref: {}",
                String::from_utf8_lossy(&before.stderr).trim()
            ));
        };
        let remote = crate::remote::Remote::open(self.root, REMOTE, run)?;
        let tip = remote.refresh_main()?;
        if let Err(why) = self.local(&["update-ref", "--no-deref", &reference, &tip, &expected]) {
            // Git's local transport may have recorded this same observation itself.
            if self.local(&["rev-parse", "--verify", &reference])? != tip {
                return Err(why);
            }
        }
        Ok(tip)
    }

    /// Record a known successful landing where the tracking ref still names its expected
    /// parent. Git's local transport may already have updated it; agh does not. Another
    /// writer's newer observation is preserved, including one made between this read and CAS.
    /// Only the tracking ref itself is written: a symbolic alias never redirects the write
    /// into its target branch, even if introduced after the read.
    fn track_landing(&self, landing: &str, onto: &str) -> Result<(), String> {
        let reference = format!("refs/remotes/{REMOTE}/{MAIN}");
        let current = self.local(&["rev-parse", "--verify", &reference])?;
        if current == onto {
            self.local(&["update-ref", "--no-deref", &reference, landing, onto])?;
        }
        Ok(())
    }

    fn is_ancestor(&self, ancestor: &str, of: &str) -> Result<bool, String> {
        let out = self.status(&["merge-base", "--is-ancestor", ancestor, of], LOCAL)?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(format!(
                "git merge-base --is-ancestor {ancestor} {of}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// The paths whose content differs between the trees of `from` and `to`.
    fn files(&self, from: &str, to: &str) -> Result<BTreeSet<Vec<u8>>, String> {
        self.paths(&[
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            from,
            to,
        ])
    }

    /// Every changed entry, including parent trees and mode or type changes.
    fn entry_changes(&self, from: &str, to: &str) -> Result<BTreeSet<Vec<u8>>, String> {
        self.paths(&[
            "diff-tree",
            "-r",
            "-t",
            "-z",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            from,
            to,
        ])
    }

    /// Read Git's NUL-delimited path protocol without trimming or decoding its data.
    fn paths(&self, args: &[&str]) -> Result<BTreeSet<Vec<u8>>, String> {
        let out = self.complete(args, LOCAL)?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(<[u8]>::to_vec)
            .collect())
    }

    /// The tree composing `checked` onto moving `main` makes ([`Compose::Tree`]), or a Git
    /// conflict with the paths it names conflicted, which requires an ordinary worktree merge.
    /// Git writes the tree and then each conflicted path once, each ended by a NUL. It is the
    /// merge of `main` into `checked`, as the landing's checkout makes it, so a union merge's
    /// lines come in the same order, `checked`'s first.
    fn merge_tree(&self, main: &str, checked: &str) -> Result<Compose, String> {
        let out = self.complete(
            &[
                "merge-tree",
                "--write-tree",
                "--no-messages",
                "--name-only",
                "-z",
                checked,
                main,
            ],
            LOCAL,
        )?;
        let mut fields = out.stdout.split(|b| *b == 0);
        let first = String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
        match out.status.code() {
            Some(0) if aterm_land_trailer::object_id(&first) => Ok(Compose::Tree(first)),
            Some(1) => Ok(Compose::Conflict(
                fields
                    .filter(|path| !path.is_empty())
                    .map(<[u8]>::to_vec)
                    .collect(),
            )),
            _ => Err(format!(
                "git merge-tree {checked} {main}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// MERGE_HEAD's commit, `None` where the checkout holds no merge.
    fn merge_head(&self) -> Result<Option<String>, String> {
        const ARGS: &[&str] = &["rev-parse", "-q", "--verify", "MERGE_HEAD"];
        let out = self.complete(ARGS, LOCAL)?;
        match out.status.code() {
            Some(0) => Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim_end().to_owned(),
            )),
            Some(1) if out.stdout.is_empty() => Ok(None),
            _ => Err(format!(
                "git {}: {}",
                ARGS.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// Whether the checkout is `tip`, committed and clean, with no merge in progress.
    fn stands_at(&self, tip: &str) -> bool {
        self.head().as_deref() == Ok(tip)
            && matches!(self.merge_head(), Ok(None))
            && self.changes().is_ok_and(|changes| changes.is_empty())
    }

    /// Whether HEAD and MERGE_HEAD are still this operation's `tip` and `main`.
    fn merging(&self, tip: &str, main: &str) -> bool {
        self.head().as_deref() == Ok(tip)
            && self
                .merge_head()
                .is_ok_and(|head| head.as_deref() == Some(main))
    }

    /// Abort only the merge this call started: both immutable endpoints must still be the
    /// checkout's merge state. An external HEAD move is never unwound. Where `merge --abort`
    /// fails on a checkout that still holds no change, `merge --quit` forgets the merge instead:
    /// git 2.43 under a held index lock writes its merge state (MERGE_HEAD, MERGE_MSG,
    /// MERGE_MODE; MERGE_RR under rerere) and ORIG_HEAD but touches neither the index nor the
    /// worktree, and its abort needs that lock. The returned diagnostic always retains the
    /// original failure and says whether the clean branch tip was restored, read after the undo.
    fn abort_own_merge(&self, tip: &str, main: &str, why: String) -> String {
        if self.stands_at(tip) {
            return format!("{why}; git wrote no merge state and the branch tip stands");
        }
        if !self.merging(tip, main) {
            return format!(
                "{why}; the merge was not aborted because HEAD/MERGE_HEAD no longer name this \
                 operation's {tip}/{main} endpoints"
            );
        }
        let undone = match self.local(&["merge", "--abort"]) {
            Ok(_) => "its uncommitted merge was aborted".to_string(),
            Err(abort)
                if self.changes().is_ok_and(|changes| changes.is_empty())
                    && self.merging(tip, main) =>
            {
                match self.local(&["merge", "--quit"]) {
                    Ok(_) => format!(
                        "git merge --abort failed ({abort}), so the merge, which had changed \
                         neither the index nor the worktree, was forgotten"
                    ),
                    Err(quit) => {
                        return format!(
                            "{why}; its uncommitted merge could not be aborted ({abort}) or \
                             forgotten ({quit})"
                        );
                    }
                }
            }
            Err(abort) => {
                return format!(
                    "{why}; its uncommitted merge could not be aborted either: {abort}"
                );
            }
        };
        if self.stands_at(tip) {
            return format!("{why}; {undone} and the branch tip restored");
        }
        format!(
            "{why}; {undone}, but the checkout did not restore cleanly (HEAD {:?}, MERGE_HEAD \
             {:?}, changes {:?})",
            self.head(),
            self.merge_head(),
            self.changes()
        )
    }

    /// Merge main commit `main` into the checkout and update its submodules. The merge is held
    /// before its commit, so a commit git refuses (a signing failure, a held `index.lock`) is
    /// aborted back to the clean branch tip ([`Self::abort_own_merge`]) rather than left as a
    /// staged merge the next run cannot start from. A conflict whose every path a mechanical
    /// rule resolves ([`resolve::plan`]) is resolved and committed with the rules named in its
    /// message; any other is left as git left it ([`Merge::Conflict`]).
    fn merge(&self, main: &str, subject: &str) -> Result<Merge, String> {
        let tip = self.head()?;
        let out = self
            .status(
                &[
                    "-c",
                    "submodule.recurse=false",
                    "merge",
                    "--no-ff",
                    "--no-commit",
                    "--no-edit",
                    "--no-verify",
                    "-m",
                    subject,
                    main,
                ],
                NETWORK,
            )
            .map_err(|why| self.abort_own_merge(&tip, main, why))?;
        let conflicted = self
            .paths(&["diff", "-z", "--name-only", "--diff-filter=U"])
            .map_err(|why| self.abort_own_merge(&tip, main, why))?;
        if !out.status.success() && conflicted.is_empty() {
            let why = format!(
                "git merge {main}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return Err(self.abort_own_merge(&tip, main, why));
        }
        let merge_head = self
            .local(&["rev-parse", "--verify", "MERGE_HEAD"])
            .map_err(|why| self.abort_own_merge(&tip, main, why))?;
        if merge_head != main {
            return Err(self.abort_own_merge(
                &tip,
                main,
                format!("git merge established MERGE_HEAD {merge_head}, expected {main}"),
            ));
        }
        let after = self
            .head()
            .map_err(|why| self.abort_own_merge(&tip, main, why))?;
        if after != tip {
            return Err(self.abort_own_merge(
                &tip,
                main,
                format!("HEAD moved from {tip} to {after} during its uncommitted merge"),
            ));
        }
        // A commit's stdout is diagnostic, not a data protocol. Its owning exit decides
        // success; complete capture remains necessary for the repository reads above.
        let commit = |args: &[&str], shown: &str| -> Result<(), String> {
            let committed = self
                .status(args, LOCAL)
                .map_err(|why| self.abort_own_merge(&tip, main, why))?;
            if !committed.status.success() {
                return Err(self.abort_own_merge(
                    &tip,
                    main,
                    format!(
                        "git {shown}: {}",
                        String::from_utf8_lossy(&committed.stderr).trim()
                    ),
                ));
            }
            Ok(())
        };
        if conflicted.is_empty() {
            const COMMIT: &[&str] = &["commit", "--quiet", "--no-verify", "--no-edit"];
            commit(COMMIT, &COMMIT.join(" "))?;
            self.update_submodules()?;
            return Ok(Merge::Clean {
                resolved: Vec::new(),
            });
        }
        // Every conflicted path's rule is asked before anything is written: one path no rule
        // resolves leaves the whole conflict as git left it.
        let resolution = match resolve::plan(self, &conflicted) {
            Ok(resolution) => resolution,
            Err(refusals) => {
                return Ok(Merge::Conflict {
                    paths: conflicted.into_iter().collect(),
                    refusals,
                });
            }
        };
        resolve::apply(self, &resolution).map_err(|why| self.abort_own_merge(&tip, main, why))?;
        let left = self
            .paths(&["diff", "-z", "--name-only", "--diff-filter=U"])
            .map_err(|why| self.abort_own_merge(&tip, main, why))?;
        if !left.is_empty() {
            let named = left
                .iter()
                .map(|path| display_git_path(path))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(self.abort_own_merge(
                &tip,
                main,
                format!("the rules left {named} unmerged"),
            ));
        }
        // An explicit message, not git's MERGE_MSG: under `--no-edit` its cleanup is
        // whitespace, which would keep its commented `# Conflicts:` lines.
        let message = format!("{subject}\n\n{}", resolution.note());
        commit(
            &["commit", "--quiet", "--no-verify", "-m", message.as_str()],
            "commit --quiet --no-verify -m <the resolved merge's message>",
        )?;
        self.update_submodules()?;
        Ok(Merge::Clean {
            resolved: resolution.said,
        })
    }

    /// Every unmerged path's stages, read from the index (`ls-files -u`): the merge base, ours
    /// and theirs ([`resolve::Stages`]).
    fn unmerged(&self) -> Result<BTreeMap<Vec<u8>, resolve::Stages>, String> {
        const ARGS: &[&str] = &["ls-files", "-u", "-z"];
        let out = self.complete(ARGS, LOCAL)?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                ARGS.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let mut found: BTreeMap<Vec<u8>, resolve::Stages> = BTreeMap::new();
        for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let malformed = || {
                format!(
                    "git {} wrote a record that is not `<mode> <object> <stage>\\t<path>`: {:?}",
                    ARGS.join(" "),
                    String::from_utf8_lossy(record)
                )
            };
            let tab = record
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(malformed)?;
            let meta = std::str::from_utf8(&record[..tab]).map_err(|_| malformed())?;
            let mut parts = meta.split(' ');
            let (Some(mode), Some(oid), Some(stage), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(malformed());
            };
            let slot = match stage {
                "1" => 0,
                "2" => 1,
                "3" => 2,
                _ => return Err(malformed()),
            };
            let stages = found
                .entry(record[tab + 1..].to_vec())
                .or_insert_with(|| [None, None, None]);
            stages[slot] = Some(resolve::Stage {
                mode: mode.to_owned(),
                oid: oid.to_owned(),
            });
        }
        Ok(found)
    }

    /// The bytes of blob `oid`.
    fn blob(&self, oid: &str) -> Result<Vec<u8>, String> {
        let args = ["cat-file", "blob", oid];
        let out = self.complete(&args, LOCAL)?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out.stdout)
    }

    /// Write `bytes` at repository path `path` in the checkout and stage it. A rule writes only
    /// a path git's merge left conflicted, so the file is git's own, at a path git checked out.
    fn put(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let file = self.root.join(path);
        std::fs::write(&file, bytes).map_err(|error| format!("{}: {error}", file.display()))?;
        self.local(&["--literal-pathspecs", "add", "--", path])
            .map(drop)
    }

    /// The rule lines of every merge of main this tool resolved by rule
    /// ([`resolve::Resolution::note`]) on the first-parent line from `onto` to `at`, oldest
    /// first, each without its `- `. Only a merge whose subject is this tool's
    /// ([`merge_subject`]) and whose second paragraph opens with [`resolve::NOTE_HEAD`] counts,
    /// so no branch commit that mentions the words is copied into a landing message. Nor does
    /// one `onto` already holds restated ([`Self::restated`]): its rules ran for that earlier
    /// landing, not this one.
    fn resolutions(&self, onto: &str, at: &str) -> Result<Vec<String>, String> {
        let range = format!("{onto}..{at}");
        let args = [
            "log",
            "--first-parent",
            "--merges",
            "--format=%x00%T %P%n%B",
            range.as_str(),
        ];
        let out = self.complete(&args, LOCAL)?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let ours = format!("Merge {REMOTE}/{MAIN} into ");
        let mut lines = Vec::new();
        for record in text.split('\0').rev() {
            let Some((identity, message)) = record.split_once('\n') else {
                continue;
            };
            let mut paragraphs = message.trim_start_matches('\n').split("\n\n");
            let (Some(subject), Some(note)) = (paragraphs.next(), paragraphs.next()) else {
                continue;
            };
            if subject.contains('\n') || !subject.starts_with(&ours) {
                continue;
            }
            let Some(rest) = note.strip_prefix(resolve::NOTE_HEAD) else {
                continue;
            };
            if !rest.starts_with('\n') || self.restated(identity, onto)? {
                continue;
            }
            lines.extend(
                rest.lines()
                    .filter_map(|line| line.strip_prefix("- "))
                    .map(str::to_owned),
            );
        }
        Ok(lines)
    }

    /// Whether `onto` holds a merge with the tree and parents `identity` names
    /// (`<tree> <first parent> <main parent>`, as `%T %P` writes them): the landing that
    /// restated that merge, on main already, as where the checkout was not moved to that
    /// landing ([`Self::adopt`]). Such a landing is a child of the merged main
    /// commit and an ancestor of `onto`, so only that ancestry path is read.
    fn restated(&self, identity: &str, onto: &str) -> Result<bool, String> {
        let Some(main) = identity.split(' ').nth(2) else {
            return Ok(false);
        };
        let range = format!("{main}..{onto}");
        Ok(self
            .local(&[
                "log",
                "--merges",
                "--ancestry-path",
                "--format=%T %P",
                range.as_str(),
            ])?
            .lines()
            .any(|line| line == identity))
    }

    /// Check out the commits the index records for this checkout's submodules. A recorded
    /// commit an initialised submodule lacks (a merged main moved its pin) is imported first,
    /// exactly, through that submodule's own [`REMOTE`]
    /// ([`crate::remote::Remote::import_commit`]), each import's remote steps held to
    /// [`NETWORK`]. An uninitialised submodule is a named prerequisite instead: `--init` could
    /// clone through raw git, even with `--no-fetch`.
    fn update_submodules(&self) -> Result<(), String> {
        self.update_submodules_through(&|root, tool, args, until| {
            crate::remote::run(root, self.make, tool, args, until)
        })
    }

    /// [`Self::update_submodules`], each remote step of an import run by `remote` in the
    /// submodule's own repository, held to its `until`.
    fn update_submodules_through(&self, remote: &SubmoduleRun<'_>) -> Result<(), String> {
        let status = self.local(&["submodule", "status", "--recursive"])?;
        // An uninitialised submodule's line is `-<commit> <path>`, with no description.
        let uninitialised: Vec<&str> = status
            .lines()
            .filter_map(|line| line.strip_prefix('-'))
            .map(|rest| rest.split_once(' ').map_or(rest, |(_, path)| path))
            .collect();
        if !uninitialised.is_empty() {
            return Err(format!(
                "initialise {} through agh before verify land",
                uninitialised.join(", ")
            ));
        }
        for (path, pin) in self.gitlinks()? {
            let root = self.root.join(&path);
            let submodule = Git {
                root: &root,
                make: self.make,
            };
            if submodule.has_commit(&pin)? {
                continue;
            }
            let until = Instant::now() + NETWORK;
            let run = |tool, args: &[std::ffi::OsString]| remote(&root, tool, args, until);
            crate::remote::Remote::open(&root, REMOTE, &run)
                .and_then(|remote| remote.import_commit(&pin, GITLINK_REF))
                .map_err(|why| format!("{path}'s pinned commit {pin} was not imported: {why}"))?;
        }
        self.ok(&["submodule", "update", "--no-fetch"], NETWORK)
            .map(drop)
    }

    /// The index's gitlinks at the paths `.gitmodules` names (the ones `git submodule update`
    /// checks out): each one's path and the commit it records. Only those paths are listed, so
    /// the read stays the size of the submodule set, not of the index.
    fn gitlinks(&self) -> Result<Vec<(String, String)>, String> {
        if !self.root.join(".gitmodules").is_file() {
            return Ok(Vec::new());
        }
        let out = self.complete(
            &[
                "config",
                "-z",
                "--file",
                ".gitmodules",
                "--get-regexp",
                r"^submodule\..*\.path$",
            ],
            LOCAL,
        )?;
        // Exit 1 is git config's "no key matched".
        if out.status.code() == Some(1) {
            return Ok(Vec::new());
        }
        if !out.status.success() {
            return Err(format!(
                "git config --file .gitmodules: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let mut args = vec![
            "ls-files".to_owned(),
            "--stage".into(),
            "-z".into(),
            "--".into(),
        ];
        for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry =
                std::str::from_utf8(entry).map_err(|_| ".gitmodules names a non-UTF-8 path")?;
            let (_, path) = entry
                .split_once('\n')
                .ok_or(".gitmodules has a path key with no value")?;
            args.push(format!(":(literal){path}"));
        }
        if args.len() == 4 {
            return Ok(Vec::new());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let listed = self.complete(&args, LOCAL)?;
        if !listed.status.success() {
            return Err(format!(
                "git ls-files --stage: {}",
                String::from_utf8_lossy(&listed.stderr).trim()
            ));
        }
        let mut links = Vec::new();
        for entry in listed.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let Some(rest) = entry.strip_prefix(b"160000 ") else {
                continue;
            };
            let rest = std::str::from_utf8(rest).map_err(|_| "a gitlink path is not UTF-8")?;
            let (meta, path) = rest
                .split_once('\t')
                .ok_or("git ls-files --stage: an entry has no path")?;
            let (pin, stage) = meta
                .split_once(' ')
                .ok_or("git ls-files --stage: an entry has no stage")?;
            if stage != "0" || !aterm_land_trailer::object_id(pin) {
                return Err(format!("{path}: the index records no one merged commit"));
            }
            links.push((path.to_owned(), pin.to_owned()));
        }
        Ok(links)
    }

    /// This repository holds commit `sha`. A partial clone is not asked to fetch it lazily.
    fn has_commit(&self, sha: &str) -> Result<bool, String> {
        let spec = format!("{sha}^{{commit}}");
        let mut cmd = (self.make)(self.root);
        cmd.env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-e", &spec]);
        let out = aterm_uds::deadline::output_while_process_progressing(&mut cmd, LOCAL, STEP)
            .map_err(|e| format!("git cat-file -e {spec}: {e}"))?;
        Ok(out.status.success())
    }

    fn commit(&self, tree: &str, parents: &[&str], message: &str) -> Result<String, String> {
        let mut args = vec!["commit-tree", tree];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        args.extend(["-m", message]);
        self.local(&args)
    }

    /// Keep the passed judgment `stated` of `judged_at` (which merges main commit `merged`) for a
    /// later run, at [`KEPT`]: a commit of the judged tree whose parents are `judged_at` and
    /// `merged` and whose message carries the receipt's trailers, with no composition named. The
    /// judged tree.
    fn keep(&self, judged_at: &str, merged: &str, stated: &Stated) -> Result<String, String> {
        let tree = self.tree(judged_at)?;
        let mut stated = stated.clone();
        stated.composed_onto = None;
        let message = format!(
            "verify land: the judgment of {tree}, kept until its landing is pushed\n\n{}",
            stated.trailers()
        );
        let record = self.commit(&tree, &[judged_at, merged], &message)?;
        self.local(&[
            "update-ref",
            "-m",
            "verify land: a judgment passed",
            KEPT,
            &record,
        ])?;
        Ok(tree)
    }

    /// The record kept at [`KEPT`]: `None` where none is kept, else the commit it judged with
    /// the judgment, or why the record is not whole. It is whole where it names a judged commit
    /// and the main commit that merged, and carries a receipt whose last judgment or round names
    /// the record's own tree and that main commit, composed onto nothing.
    fn kept_record(&self) -> Result<Option<KeptRecord>, String> {
        let spec = format!("{KEPT}^{{commit}}");
        let found = self.complete(&["rev-parse", "-q", "--verify", &spec], LOCAL)?;
        let record = match found.status.code() {
            Some(0) => String::from_utf8_lossy(&found.stdout).trim_end().to_owned(),
            Some(1) if found.stdout.is_empty() => return Ok(None),
            _ => {
                return Err(format!(
                    "git rev-parse {spec}: {}",
                    String::from_utf8_lossy(&found.stderr).trim()
                ));
            }
        };
        let parents = self.parents(&record)?;
        let [judged_at, merged] = parents.as_slice() else {
            return Ok(Some(Err(format!(
                "{record} at {KEPT} is not a judged commit and the main commit it merged"
            ))));
        };
        let judged = self.tree(&record)?;
        let text = self.local(&["log", "-1", "--format=%B", &record])?;
        let stated = Stated::of_trailers(&text).filter(|stated| {
            let last = stated
                .rechecks_since_judged()
                .last()
                .map(|recheck| (recheck.tree.as_str(), recheck.onto.as_str()))
                .or_else(|| {
                    stated
                        .checked
                        .last()
                        .map(|checked| (checked.tree.as_str(), checked.base.as_str()))
                });
            stated.composed_onto.is_none() && last == Some((judged.as_str(), merged.as_str()))
        });
        let Some(stated) = stated else {
            return Ok(Some(Err(format!(
                "{record} at {KEPT} does not state a judgment of its own tree over the cone \
                 since {merged}"
            ))));
        };
        Ok(Some(Ok((
            judged_at.clone(),
            Kept {
                tree: judged,
                merged: merged.clone(),
                stated,
            },
        ))))
    }

    /// The judgment kept at [`KEPT`], where it holds for the checkout at `head`: `None` where
    /// none is kept, else the judgment or why this run does not resume it. It holds only where
    /// its record is whole ([`Self::kept_record`]), `head` is the judged commit and holds the
    /// judged tree, and `head` holds the main commit it merged: a judgment passes only on the
    /// tree it ran on, over the cone it was computed from.
    fn kept(&self, head: &str) -> Result<Option<Result<Kept, String>>, String> {
        let (judged_at, kept) = match self.kept_record()? {
            None => return Ok(None),
            Some(Err(why)) => return Ok(Some(Err(why))),
            Some(Ok(record)) => record,
        };
        if judged_at != head {
            return Ok(Some(Err(format!(
                "it judged {judged_at}, and HEAD is {head}"
            ))));
        }
        let tree = self.tree(head)?;
        if tree != kept.tree {
            return Ok(Some(Err(format!(
                "it judged {}, and HEAD holds {tree}",
                kept.tree
            ))));
        }
        if !self.is_ancestor(&kept.merged, head)? {
            return Ok(Some(Err(format!(
                "HEAD does not hold {}, the main commit it merged",
                kept.merged
            ))));
        }
        Ok(Some(Ok(kept)))
    }

    /// The judgment kept at [`KEPT`], where the checkout at `head` is a merge of a main commit
    /// into the commit it judged: the judgment and that main commit. It holds only where the
    /// record is whole ([`Self::kept_record`]), `head` has exactly two parents, the first the
    /// judged commit, holding the judged tree, and the second a commit that descends from the
    /// main the judgment merged and that `fetched`, main's tip, holds. Such a merge is judged
    /// as a round ([`recheck`]), whoever made it: a person, after a conflict stopped the
    /// landing or by merging main themselves, or this tool, in a run that ended before its
    /// round of the merge passed. A merge the tool began to judge in full is not found here:
    /// that judgment dropped the record before it ran, so a red or killed full judgment is
    /// never followed by a round. Nothing here says the merge is one a round answers for:
    /// [`round_delta`] decides that.
    fn kept_merge(&self, head: &str, fetched: &str) -> Result<Option<(Kept, String)>, String> {
        let Some(Ok((judged_at, kept))) = self.kept_record()? else {
            return Ok(None);
        };
        let parents = self.parents(head)?;
        let [first, onto] = parents.as_slice() else {
            return Ok(None);
        };
        if *first != judged_at
            || self.tree(first)? != kept.tree
            || !self.is_ancestor(&kept.merged, onto)?
            || !self.is_ancestor(onto, fetched)?
        {
            return Ok(None);
        }
        Ok(Some((kept, onto.clone())))
    }

    /// The tree of the judgment kept at [`KEPT`], where HEAD is a merge whose first parent is
    /// the commit it judged: a round of that merge that went red is run again as a round while
    /// the merge keeps those parents (amended, never built upon). `None` where nothing is kept
    /// for HEAD's first parent.
    fn kept_parent(&self) -> Result<Option<String>, String> {
        let Some(Ok((judged_at, kept))) = self.kept_record()? else {
            return Ok(None);
        };
        let parents = self.parents(&self.head()?)?;
        Ok(matches!(parents.as_slice(), [first, _] if *first == judged_at).then_some(kept.tree))
    }

    /// Forget the kept judgment ([`KEPT`]): its landing is on main, or a whole judgment is
    /// about to answer for the checkout instead. After a landing a record that does not
    /// delete holds for no checkout whose head is not already on main; before a whole
    /// judgment it must be gone, so that caller stops on the error.
    fn forget(&self) -> Result<(), String> {
        self.local(&["update-ref", "-d", KEPT]).map(drop)
    }

    /// Move the checkout's branch from `judged` to the landing: a fast-forward where the landing
    /// descends from it, else (the same tree, restated) the ref alone. An error says how far the
    /// checkout moved.
    fn adopt(&self, landing: &str, judged: &str) -> Result<(), String> {
        let not_moved = |why: String| format!("this checkout was not moved to it: {why}");
        if self
            .parents(landing)
            .map_err(not_moved)?
            .first()
            .map(String::as_str)
            == Some(judged)
        {
            self.ok(
                &[
                    "-c",
                    "submodule.recurse=false",
                    "merge",
                    "--ff-only",
                    "--quiet",
                    landing,
                ],
                NETWORK,
            )
            .map_err(not_moved)?;
            return self.update_submodules().map_err(|why| {
                format!(
                    "this checkout's submodules were not updated to it: {why}. Fix: \
                         initialise any uninitialised submodule and import any named pinned \
                         commit through agh, then run `git submodule update --no-fetch`"
                )
            });
        }
        // Git resolves HEAD's exact referent, including names that cannot round-trip through
        // text. The same transaction also handles a detached HEAD and checks its old commit.
        self.local(&["update-ref", "-m", "verify land", "HEAD", landing, judged])
            .map(drop)
            .map_err(not_moved)
    }
}

/// `verify land`'s usage.
#[must_use]
pub fn usage() -> String {
    format!(
        "usage: targo --unverified verify land [--tab | --at <commit>] [--admit wait|now] [--timeout <seconds|off>] [--test-cores <n>]\n\
                {records_usage}\n\
                {census_usage}\n\n\
         Run directly with your shell/background command tools by default. Use --tab only\n\
         on the person's explicit request or for a concrete persistence need those tools\n\
         cannot meet, such as an actual restart or upgrade that will end the launching\n\
         process. State that reason before spawning the tab.\n\
         --tab, in a shell of an aterm session, starts this landing (its other\n\
         words, with --at HEAD) as a program worker of that session in a tab of its own at the\n\
         checkout's root, prints that nothing has landed yet, the line that waits for the\n\
         worker however long it runs (aterm ctl --timeout 0 await task @<sid> since=0) and the\n\
         one that reads its verdict (aterm ctl @<sid> result), and exits {started}: the landing\n\
         outlives that session's agent, its restart, usage limit or upgrade. Leave the\n\
         checkout's HEAD and files as they are until the worker ends. A checkout with changes\n\
         no commit holds, or one a landing runs in, starts nothing (exit 3). With no aterm\n\
         harness to run it (no aterm session, or ERR unsupervised) it lands here, as without\n\
         the flag; any other answer lands nowhere and exits 3.\n\
         --at <commit> lands that commit or nothing: a HEAD the landing reads that is not it\n\
         could not run.\n\
         One landing runs in a checkout at a time: it holds {lock} in the checkout's git\n\
         directory while it runs, and another refuses at once (exit 3).\n\n\
         Land this checkout's branch on {REMOTE}/{MAIN}, judged: merge {REMOTE}/{MAIN} (M1),\n\
         apply census --fix and the formatter over what the branch changed and commit what they\n\
         wrote into that merge (each re-judge's merge too), run the dev set over the cone since\n\
         M1, push, and where {MAIN} moved meanwhile compose onto\n\
         it (a clean merge and disjoint or exactly convergent entries, except a sole remaining\n\
         shared Cargo.lock whose isolated local product version alone moved) or merge it and\n\
         judge the branch's delta from that main tip. A {MAIN} that moves again is composed\n\
         onto again, at most {COMPOSES} times in a row. No count of re-judges ends a landing:\n\
         a red, a conflict, a checkout that moved, that push bound, a remote that stays down\n\
         or a hang detector does. Where the endpoints share any other path and the merge is\n\
         clean, the judgment stands for the tree it ran on and a ROUND judges the merge: it\n\
         merges that tip and runs, with the merged tree's own verifier, the dev set since\n\
         the tip over the crates of the branch's cone that {MAIN}'s change reaches: of the\n\
         crates a judgment of the merge would select, those in the cone of the shared paths\n\
         and those in the cone of what {MAIN} changed since the judged commit, shared or\n\
         not. A crate left out builds from the judged tree's own files. fmt, census and\n\
         export-scan read the branch's whole change and must each pass. The round states an\n\
         Aterm-Rechecked line and renews the push bound; a red there counts as a judgment's\n\
         does. Shared Markdown no build, include or test reads (root *.md, docs/**/*.md, a\n\
         workspace member's SPECS.md; census's no-reach-read guard holds that set) is in no\n\
         cone, and a round of a clean merge that shares only such Markdown, which no code\n\
         names, judges no crate. Where code names it, it stands for the files that name it,\n\
         as a shared code path stands for itself: the round judges, of the branch's cone,\n\
         each one's crate (with its dependents unless only that crate's tests compile the\n\
         file) and what {MAIN}'s change reaches. Where one of those files is no workspace\n\
         member's, where the Markdown is no regular file on every side, where the round\n\
         would judge every crate the branch's change reaches ({MAIN}'s change, with those\n\
         files, reaches them all or none of them, or the branch's reaches every crate) or\n\
         where {MAIN} was rewritten, the tip is merged and the branch judged in full. Where\n\
         the merge CONFLICTS the judgment is kept: the merge stops on the conflict, and once a\n\
         person commits it (first parent the judged commit, second a commit of {MAIN}) the\n\
         next run judges it by a round, in which no shared path's crate is judged alone; a\n\
         hand merge that shares a lock, a root manifest, a toolchain or lint file or a\n\
         submodule entry, or Markdown a file may read, is judged in full, and a conflict in\n\
         such a path keeps nothing.\n\
         Each judgment and round STOPS at its first red, which counts whatever {MAIN}'s own\n\
         state, and names what that stopped. The landing merge carries Aterm-Checked,\n\
         Aterm-Interrupted, Aterm-Rechecked and Aterm-Composed-Onto trailers.\n\n\
         A judgment's events go to its events file, not the console; the landing ends with one\n\
         VERDICT block: landed or not, each check's word and time, the first red and where,\n\
         what the first red stopped, the full log and `verify status --events` on the events\n\
         kept beside it.\n\n\
         While a judgment runs, {REMOTE}/{MAIN} is read at each stage boundary the run reports\n\
         and at least every {poll} minutes. The first tip a full judgment must answer by the\n\
         same rule (a conflicting one only where the merge to come leaves nothing of the\n\
         judgment standing) INTERRUPTS the run: its process group is ended, and what that started outside\n\
         it is awaited as it ends by its own parent-death watch. What of that still runs\n\
         {grace}s after the last of it ended is named and awaited {bound}s more; where it\n\
         runs even then the landing ends COULD NOT RUN, since nothing is judged beside a\n\
         process that may still write the build directory. The interrupted run is no\n\
         verdict, and states an Aterm-Interrupted line, never an Aterm-Checked one. That tip\n\
         is merged and judged at once. A tip\n\
         that composes or that a round answers changes nothing, and a run that already reported a red (FAIL or NOT RUN:\n\
         a check, a stage or a test binary) is not interrupted: its end decides. A read of a tip the last composed one already holds (a stale read, or a\n\
         {MAIN} rewound by force) is said once and never interrupts: the judgment goes on, and a\n\
         {MAIN} that stays rewound refuses the push, so nothing is pushed over it.\n\n\
         Convergence requires identical mode, type and object (or all absent) in checked, new\n\
         main and merge, including parent trees. Disjoint changes need no extra entry reads;\n\
         unavailable evidence retains the original overlap and lock rule.\n\n\
         The lock exception validates four immutable trees, preserving every other lock byte,\n\
         dependency, format and mode. Uncertain evidence is judged again. Aterm-Checked still\n\
         names the tree that ran; a composed product identity is not that earlier executable.\n\n\
         --admit changes resource admission only, forwarded to every judgment; wait is the\n\
         default. --timeout changes only each judgment's child-work ceiling (3h by default);\n\
         off removes it. --test-cores caps the CPU share requested for test execution;\n\
         admission may grant less. The full dev set and change cone are unchanged.\n\n\
         A judgment that passed is never lost to the push. The count above is the push's\n\
         only bound: no deadline spans its landings, and each remote step runs under its own\n\
         hang detector ({network}s; {write}s for a write, which agh's own bounds come before).\n\
         A remote step that failed or timed out having sent nothing (a read of {MAIN}, a push\n\
         agh says it did not send) is made again after a pause that doubles from {pause}s, at\n\
         most {RETRIES} times in a row; a push whose outcome is unknown is never made again,\n\
         and a {MAIN} that holds its landing took it. The judgment is kept in this worktree\n\
         ({KEPT}) until its landing is on {MAIN}: a run that\n\
         ends before then (stopped at a bound, a remote that stayed down, a kill) is run again,\n\
         and while HEAD is still the judged commit it goes straight to the push with the same\n\
         receipt, judging again only where the moved {MAIN} does not compose. While HEAD is a\n\
         merge of a {MAIN} commit into the judged commit, it starts at that merge's round: a\n\
         red round is fixed by amending the merge, since a commit on top of it is judged in\n\
         full. A full judgment of a merge drops what was kept before it runs, so one that\n\
         went red or was killed is run again in full.\n\n\
         Each check's verdict is recorded under refs/aterm/judged on {REMOTE}\n\
         (land/<day>/<tree>/), signed with this Mac's fleet node key: each FAIL of a judgment\n\
         or a round that ended by itself in its checkout, and where its run was whole each\n\
         other row its plan keyed, each kept at least two days. Beside them, under\n\
         land/runs/<day>/, one run record for each judgment, round, compose and abort, and one\n\
         for the run's end (an interrupted or panicked run's included; one killed outright, or\n\
         whose step outlasts the interrupt's grace, has none), each kept at least fourteen days. Each record is kept in this clone as it is made; one append\n\
         at the run's end, after its last use of {MAIN}, carries them with any an earlier run\n\
         left, and a failed append is said and made by the next run, changing no exit and no\n\
         count. `verify land records` lists a tree's verdicts; `verify land --census` reads the\n\
         run records. Nothing reads them to decide a landing: what lands, and the exit, are the\n\
         same with them as without them.\n\n\
         GitHub reads and writes use agh (expected refs, durable write intents; one write, with\n\
         no rehearsal that would repeat agh's re-verification of the pushed history while\n\
         {MAIN} moves); local remotes use git's file protocol only. Required submodules must\n\
         already be initialised:\n\
         landing never clones one. A commit a merged main pins that a submodule lacks is imported,\n\
         exactly, through that submodule's own {REMOTE}, the same way, and is all it fetches there.\n\n\
         Each judgment and recheck runs as the verifier built from the tree it judges: where that\n\
         build is not the running landing, the landing hands itself to it in place and goes on\n\
         ({resume} <file> is that handoff's own word, never a person's).\n\n\
         A merge of {MAIN} (M1, or a re-judge's) that conflicts is resolved only where every\n\
         conflicted path has a mechanical rule that resolves it: docs/PLAN.md and\n\
         docs/STATUS.md, which the lab tracker regenerates, take {MAIN}'s side;\n\
         a SPECS.md table takes each row from the side that changed it (a row both sides\n\
         changed differently has no rule). The resolved merge names the rules that ran and is\n\
         judged in full as any other, and the landing message names them too. One path no rule\n\
         resolves resolves none, and the merge is left for the person.\n\n\
         Exit: 0 landed (or nothing to land), 1 a red (nothing pushed), 2 usage, 3 could not\n\
         run, {conflict} a conflict no rule resolves (nothing pushed; the merge is left for the\n\
         person), {started} --tab's worker started (nothing landed yet), {STOPPED} stopped after\n\
         {COMPOSES} push races while {MAIN} kept moving (merged and judged, its judgment kept;\n\
         not pushed).\n",
        started = STARTED,
        lock = LOCK_FILE,
        conflict = exit::CONFLICT,
        resume = handoff::RESUME,
        records_usage = records::RECORDS_USAGE
            .strip_prefix("usage: ")
            .unwrap_or(records::RECORDS_USAGE),
        census_usage = crate::land_census::USAGE
            .strip_prefix("usage: ")
            .unwrap_or(crate::land_census::USAGE),
        poll = POLL.as_secs() / 60,
        grace = INTERRUPT_GRACE.as_secs(),
        bound = UNENDED_BOUND.as_secs(),
        pause = RETRY_PAUSE.as_secs(),
        network = NETWORK.as_secs(),
        write = WRITE.as_secs()
    )
}

/// Resource controls retained for every landing judgment and re-judgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandResources {
    /// How each judgment asks for capacity.
    pub admit: aterm_capacity::Admit,
    /// Its child-work hang ceiling; `None` retains verify's default.
    pub timeout: Option<String>,
    /// A ceiling on requested test cores, never a capacity grant.
    pub test_cores: Option<std::num::NonZeroU32>,
}

fn options(args: &[String]) -> Option<LandResources> {
    let mut options = LandResources {
        admit: aterm_capacity::Admit::Wait,
        timeout: None,
        test_cores: None,
    };
    let mut admit_seen = false;
    let mut timeout_seen = false;
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        if flag == aterm_capacity::Admit::FLAG {
            if admit_seen {
                return None;
            }
            let value = args.get(index + 1)?;
            options.admit = aterm_capacity::Admit::parse(value)?;
            admit_seen = true;
            index += 2;
            continue;
        }
        if let Some(value) = flag.strip_prefix("--admit=") {
            if admit_seen {
                return None;
            }
            options.admit = aterm_capacity::Admit::parse(value)?;
            admit_seen = true;
            index += 1;
            continue;
        }
        if flag == "--timeout" {
            if timeout_seen {
                return None;
            }
            let value = args.get(index + 1)?;
            crate::exec::parse_ceiling(value)?;
            options.timeout = Some(value.clone());
            timeout_seen = true;
            index += 2;
            continue;
        }
        if let Some(value) = flag.strip_prefix("--timeout=") {
            if timeout_seen {
                return None;
            }
            crate::exec::parse_ceiling(value)?;
            options.timeout = Some(value.to_owned());
            timeout_seen = true;
            index += 1;
            continue;
        }
        if flag == "--test-cores" {
            if options.test_cores.is_some() {
                return None;
            }
            options.test_cores = Some(args.get(index + 1)?.parse().ok()?);
            index += 2;
            continue;
        }
        if let Some(value) = flag.strip_prefix("--test-cores=") {
            if options.test_cores.is_some() {
                return None;
            }
            options.test_cores = Some(value.parse().ok()?);
            index += 1;
            continue;
        }
        return None;
    }
    Some(options)
}

/// `verify land [--tab | --at commit] [--admit wait|now] [--timeout seconds|off] [--test-cores n]
/// [--help]`, from `cwd`'s checkout.
/// `verify land --resume <file>` is the landing's own handoff to another build of itself.
pub fn run(args: &[String], cwd: &Path) -> i32 {
    match args {
        [help] if help == "--help" || help == "-h" => {
            print!("{}", usage());
            return exit::PASS;
        }
        [flag, file] if flag == handoff::RESUME => {
            return resume_cli(
                Path::new(file),
                cwd,
                &mut |line| eprintln!("{line}"),
                &mut records::live,
            );
        }
        [verb, rest @ ..] if verb == "records" => return records::records_cli(rest, cwd),
        [flag, rest @ ..] if flag == crate::land_census::FLAG => {
            return crate::land_census::census_cli(rest, cwd);
        }
        _ => {}
    }
    let Some((options, (in_tab, at, words))) =
        split(args).and_then(|split| Some((options(&split.2)?, split)))
    else {
        eprint!(
            "verify land: takes only --tab or --at <commit>, --admit wait|now, --timeout \
             seconds|off and --test-cores n\n{}",
            usage()
        );
        return exit::USAGE;
    };
    if crate::git_provider::pin().is_none() {
        eprintln!("verify land: COULD NOT RUN — no absolute executable Git on PATH");
        return exit::COULD_NOT_RUN;
    }
    let Some(root) = crate::checkout_root(cwd) else {
        eprintln!(
            "verify land: COULD NOT RUN — {} is not inside a git checkout",
            cwd.display()
        );
        return exit::COULD_NOT_RUN;
    };
    let git = Git {
        root: &root,
        make: &live_git,
    };
    if in_tab && let Some(code) = ask_for_worker(&git, &words) {
        return code;
    }
    let Some(image) = crate::report::running_driver() else {
        eprintln!("verify land: COULD NOT RUN — this landing's own image could not be read");
        return exit::COULD_NOT_RUN;
    };
    let _held = match git.hold() {
        Ok(held) => held,
        Err(why) => {
            eprintln!("verify land: COULD NOT RUN — {why}");
            return exit::COULD_NOT_RUN;
        }
    };
    let own = Own {
        words: &words,
        image,
        at: at.as_deref(),
    };
    interruptible(|| with_live_seams(&root, &options, |seams| land(&root, &own, seams)))
}

/// `run`'s words apart: whether `--tab` asks for a worker, the commit `--at` names, and the
/// landing's other words, which a handoff carries; `None` where either is given twice, `--at`
/// names nothing, or both are given (`--tab` names the commit itself).
fn split(args: &[String]) -> Option<(bool, Option<String>, Vec<String>)> {
    let (mut in_tab, mut at, mut words) = (false, None, Vec::new());
    let mut rest = args.iter();
    while let Some(word) = rest.next() {
        let commit = if word == tab::AT {
            Some(rest.next()?.clone())
        } else {
            word.strip_prefix("--at=").map(str::to_owned)
        };
        if let Some(commit) = commit {
            if commit.is_empty() || at.replace(commit).is_some() {
                return None;
            }
        } else if word == tab::FLAG {
            if std::mem::replace(&mut in_tab, true) {
                return None;
            }
        } else {
            words.push(word.clone());
        }
    }
    (!(in_tab && at.is_some())).then_some((in_tab, at, words))
}

/// `verify land --tab`'s ask ([`tab::start`]) for the checkout `git` holds, clean, with no
/// landing in it, and its HEAD: `Some` exit where nothing lands here ([`STARTED`] once the
/// worker runs it), `None` where the landing runs here.
fn ask_for_worker(git: &Git<'_>, words: &[String]) -> Option<i32> {
    // The lock first: a landing that runs is left alone, even by `git status`, whose index
    // refresh may take the index lock a step of that landing needs.
    let ready = || -> Result<String, String> {
        drop(git.hold()?);
        let dirty = git.changes()?;
        if !dirty.is_empty() {
            return Err(format!(
                "the checkout has changes no commit holds:\n{dirty}\nFix: commit or remove them, \
                 then run it again"
            ));
        }
        git.head()
    };
    let head = match ready() {
        Ok(head) => head,
        Err(why) => {
            eprintln!("verify land: COULD NOT RUN — {why}");
            return Some(exit::COULD_NOT_RUN);
        }
    };
    let env = |key: &str| std::env::var(key).ok();
    match tab::start(git.root, words, &head, &env, &mut live_ctl) {
        tab::Start::Worker(sid) => {
            for line in tab::said(&sid, git.root) {
                println!("{line}");
            }
            Some(STARTED)
        }
        tab::Start::Here(why) => {
            println!("verify land: {why}: the landing runs here");
            None
        }
        tab::Start::Unknown(why) => {
            eprintln!(
                "verify land: COULD NOT RUN — asked for the landing's worker, {why}; nothing was \
                 landed here. `aterm ctl board` names a worker that started; without {} the \
                 landing runs in this shell",
                tab::FLAG
            );
            Some(exit::COULD_NOT_RUN)
        }
    }
}

/// Run `landing` under verify's own interrupt handling (`exec::group::kill_on_interrupt`): a
/// `SIGINT`, `SIGTERM` or `SIGHUP` is taken, the landing stops at its next check of it and writes
/// its end record, and the process then dies of the signal (`exec::group::end_interrupted`),
/// held to `exec::group::REPORT_GRACE` by its watchdog. The checks: each [`BOUNDARY_TICK`] while
/// a judgment or a round runs; each `aterm_uds::deadline::STOP_TICK` while the checkout's
/// verifier builds, a reach is read, or the push phase reads main, reads the journal or waits;
/// at each step's start, before a merge and before a push or a handoff. A write of main runs to
/// its end and holds the watchdog until it has (`exec::group::hold`). The local git steps, the
/// checkout's toolchain and plan reads and a merge's submodule imports are not cut short: one
/// that outlasts the grace dies with the run and leaves it no end record.
fn interruptible(landing: impl FnOnce() -> i32) -> i32 {
    crate::exec::group::kill_on_interrupt();
    let code = landing();
    crate::exec::group::end_interrupted();
    code
}

/// `verify land --resume <file>`: take the handoff at `file` (removing it), then resume the
/// landing it holds in `cwd`'s checkout (the checkout's root, where the landing handed itself
/// over) as this image, under the person's `land` words parsed again by this build. Only the
/// checkout's own handoff file is read or removed ([`own_handoff`]). A handoff this build does
/// not read, or words it does not take, could not run: the person typed nothing to this image.
/// Each line such a refusal says goes to `refuse` (stderr, from [`run`]); once resumed, the
/// landing says its lines as [`land`] does.
///
/// A refusal once the handoff is taken ends the run it carried, where it carried one that
/// reads ([`Handoff::run_of`]): its end record, could not run, through the checkout's
/// `records` ([`records::live`] in production), as [`resume`] would have written it.
fn resume_cli(
    file: &Path,
    cwd: &Path,
    refuse: &mut dyn FnMut(&str),
    records: &mut dyn FnMut(&Path) -> Records<'static>,
) -> i32 {
    if crate::git_provider::pin().is_none() {
        refuse(&format!(
            "verify land: COULD NOT RUN — no absolute executable Git on PATH; {RERUN}"
        ));
        return exit::COULD_NOT_RUN;
    }
    let Some(root) = crate::checkout_root(cwd) else {
        refuse(&format!(
            "verify land: COULD NOT RUN — {} is not inside a git checkout; {RERUN}",
            cwd.display()
        ));
        return exit::COULD_NOT_RUN;
    };
    let git = Git {
        root: &root,
        make: &live_git,
    };
    if let Err(why) = own_handoff(&git, file) {
        refuse(&format!("verify land: COULD NOT RUN — {why}"));
        return exit::COULD_NOT_RUN;
    }
    let taken = Handoff::take_bytes(file);
    let decoded = match &taken {
        Ok(bytes) => Handoff::decode(bytes),
        Err(why) => Err(why.clone()),
    };
    let handoff = match decoded {
        Ok(handoff) => handoff,
        Err(why) => {
            refuse(&format!(
                "verify land: COULD NOT RUN — the landing's handoff {}: {why}; {RERUN}",
                file.display()
            ));
            let run = taken.ok().and_then(|bytes| Handoff::run_of(&bytes));
            return refused(&git, run.as_ref(), records, refuse);
        }
    };
    // The sender's lock ended with its exec: take it again before anything of the landing runs.
    let _held = match git.hold() {
        Ok(held) => held,
        Err(why) => {
            refuse(&format!("verify land: COULD NOT RUN — {why}; {RERUN}"));
            return refused(&git, handoff.run.as_ref(), records, refuse);
        }
    };
    let Some(image) = crate::report::running_driver() else {
        refuse(&format!(
            "verify land: COULD NOT RUN — this landing's own image could not be read; {RERUN}"
        ));
        return refused(&git, handoff.run.as_ref(), records, refuse);
    };
    let words = handoff.words.clone();
    let Some(options) = options(&words) else {
        refuse(&format!(
            "verify land: COULD NOT RUN — this tree's verify land does not take the landing's \
             words `{}`; {RERUN}",
            words.join(" ")
        ));
        return refused(&git, handoff.run.as_ref(), records, refuse);
    };
    interruptible(|| {
        with_live_seams(&root, &options, |seams| {
            resume(
                &root,
                &Own {
                    words: &words,
                    image,
                    at: None,
                },
                handoff,
                seams,
            )
        })
    })
}

/// End the run `carried` (where a refused handoff carried one) as [`resume`] ends one that
/// could not run: its end record, through `records` of the checkout ([`records::finish`]), each
/// line it says going to `say`. [`exit::COULD_NOT_RUN`].
fn refused(
    git: &Git<'_>,
    carried: Option<&handoff::Carried>,
    records: &mut dyn FnMut(&Path) -> Records<'static>,
    say: &mut dyn FnMut(&str),
) -> i32 {
    if let Some(carried) = carried {
        let mut records = records(git.root);
        records.adopt(carried);
        records::finish(git, &mut records, say, exit::COULD_NOT_RUN);
    }
    exit::COULD_NOT_RUN
}

/// Whether `file` is the checkout's own handoff file ([`Git::handoff_file`]), its directory
/// compared physically: the only file `--resume` reads or removes. Else why not, leaving
/// `file` as it was.
fn own_handoff(git: &Git<'_>, file: &Path) -> Result<(), String> {
    let expected = git.handoff_file()?;
    let physical = |path: &Path| {
        let name = path.file_name()?;
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        Some(std::fs::canonicalize(dir).ok()?.join(name))
    };
    match (physical(file), physical(&expected)) {
        (Some(file), Some(expected)) if file == expected => Ok(()),
        _ => Err(format!(
            "{} is not this checkout's handoff, {}: it was left as it was. `{}` is the landing's \
             own word to the image it hands itself to; to land, run `targo --unverified verify \
             land`",
            file.display(),
            expected.display(),
            handoff::RESUME
        )),
    }
}

/// Whether this process took an interrupt (`exec::group::taken`): the live steps that read or
/// wait end at it ([`interrupted`], [`live_verifier`], [`live_reach`]), so the landing reaches
/// its end record within the report grace.
fn taken() -> bool {
    crate::exec::group::taken().is_some()
}

/// A git command in the checkout at `root`, as every live landing step runs one.
fn live_git(root: &Path) -> Command {
    let mut cmd = crate::git_provider::command();
    cmd.current_dir(root)
        .env_remove("GITHUB_TOKEN")
        .env("GIT_TERMINAL_PROMPT", "0");
    for key in PATHSPEC_MODES {
        cmd.env_remove(key);
    }
    cmd
}

/// `aterm ctl <args>` from `PATH`, `stdin` its standard input: what it wrote to stdout and
/// stderr ([`tab::start`]). A write the client did not take (it answered before reading) is no
/// error: its answer says what it did.
fn live_ctl(args: &[String], stdin: &[u8]) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Write as _;
    let mut child = aterm_uds::spawnfd::command("aterm")
        .arg("ctl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut input) = child.stdin.take() {
        let _ = input.write_all(stdin);
    }
    let said = child.wait_with_output()?;
    Ok((said.stdout, said.stderr))
}

/// Run `f` with the live seams under the person's resource controls `options`.
fn with_live_seams<R>(
    root: &Path,
    options: &LandResources,
    f: impl FnOnce(&mut Seams<'_>) -> R,
) -> R {
    let mut say = |line: &str| println!("{line}");
    let records = records::live(root);
    if records.signer.is_none() {
        say(
            "verify land: this Mac has no fleet node identity: its verdict records are written \
             unsigned",
        );
    }
    f(&mut Seams {
        git: &live_git,
        verifier: &mut live_verifier,
        judge: &mut |root, verifier, base, poll| nested(root, verifier, base, None, options, poll),
        records,
        reach: &live_reach,
        recheck: &mut |root, verifier, base, delta| {
            live_recheck(root, verifier, base, delta, options)
        },
        autofix: &mut live_autofix,
        hand_over: &mut handoff::hand_over,
        say: &mut say,
        remote: &live_remote,
        pause: &live_pause,
    })
}

/// One remote step of the push phase ([`Seams::remote`]): [`crate::remote::run`], ended at the
/// interrupt the landing took ([`interrupted`]), so a read of main or a wait on another writer
/// ends at it. An explicitly tagged mutation requests cancellation from the raw signal and
/// retains its native owner through settlement while the watchdog is held ([`Wire::write`]).
fn live_remote(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: crate::remote::Tool,
    args: &[std::ffi::OsString],
    until: Instant,
    mode: RemoteMode,
) -> Result<std::process::Output, String> {
    match mode {
        RemoteMode::Read => {
            crate::remote::run_unless(root, git, tool, args, until, &mut interrupted)
        }
        RemoteMode::Mutation => {
            crate::remote::run_mutation(root, git, tool, args, until, &mut taken)
        }
    }
}

/// Wait out a pause of the push phase ([`Seams::pause`]): a retry's, or a tick of a wait on
/// another writer's open intent; one an interrupt the landing took cuts short, so the step after
/// it reads the interrupt at once ([`interrupted`]).
fn live_pause(pause: Duration) {
    let until = Instant::now() + pause;
    while !interrupted() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(aterm_uds::deadline::STOP_TICK));
    }
}

/// Whether a step of the landing that does not run to its end stops now: the landing took an
/// interrupt (`exec::group::taken`) and no write of main holds the watchdog
/// (`exec::group::held`).
fn interrupted() -> bool {
    taken() && !crate::exec::group::held()
}

/// The checkout's verifier, freshly built from it as it stands by its own toolchain (its
/// `trust.lock`), and that executable's sha256.
fn live_verifier(root: &Path) -> Result<Image, String> {
    let env = crate::EnvSnapshot::capture();
    let tools = crate::preflight(&env, root)?;
    // This is the native driver, bypassing atpkg's shim. Place its bootstrap too:
    // main_targo only places children after the nested verifier has already compiled.
    let plan = crate::NativeBuildPlan::capture(&env, root)
        .map_err(|error| format!("cannot resolve the build plan: {error}"))?;
    let path = crate::native_verifier::build(root, &tools.targo, &plan, &mut taken)?;
    let digest = crate::test_index::digest(&path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
    Ok(Image { path, digest })
}

/// The landing's fixes on the checkout at `root` as it stands ([`Seams::autofix`]), as `verify
/// census --fix` and `verify fmt --fix` make them: census's first ([`crate::census_fix`]: each
/// inline test module a build without tests reads moves out of line, and each file it wrote is
/// formatted), then the formatter's over the Rust sources among `files`, each at its package's
/// edition ([`crate::fmt::write`]; never under `vendor/` or in [`crate::fmt::EXCLUDED`], as the
/// fmt check reads none). Else what kept either from running whole.
fn live_autofix(root: &Path, files: &[String]) -> Result<(), String> {
    let scratch = aterm_tempfile::tempdir()
        .map_err(|error| format!("cannot create a scratch directory: {error}"))?;
    let ctx = crate::Ctx::new(
        root.to_path_buf(),
        crate::scope::Scope::workspace(),
        crate::EnvSnapshot::capture(),
        scratch.path().to_path_buf(),
    )
    .with_fix(true);
    let mut report = crate::ladder::Report::new("the landing's fixes");
    crate::census_fix::run(&ctx, &mut report);
    let mut why: Vec<String> = report
        .outcomes()
        .filter(|(outcome, _)| *outcome != crate::ladder::Outcome::Ok)
        .map(|(_, label)| label.to_owned())
        .collect();
    let sources: Vec<String> = files
        .iter()
        .filter(|file| {
            file.ends_with(".rs")
                && !file.starts_with("vendor/")
                && !crate::fmt::EXCLUDED.contains(&file.as_str())
                && root.join(file).is_file()
        })
        .cloned()
        .collect();
    match ctx.tools.trustfmt.clone() {
        Some(bin) => why.extend(crate::fmt::write(&ctx, &bin, &sources)),
        None if !sources.is_empty() => why.push(ctx.tools.missing_trustfmt_label()),
        None => {}
    }
    if why.is_empty() {
        Ok(())
    } else {
        Err(why.join("; "))
    }
}

/// A round: the dev set since `base` over the crates of that cone that `delta`'s shared paths
/// or main's change since its judged commit reach, with those the files that may read its
/// shared Markdown reach (`crate::changed::resolve_round`), run as [`nested`] runs a judgment:
/// as `verifier`, the checkout's own verifier
/// built from the merge it judges and the landing's own image, so the merged tree's rules
/// judge it and nothing an earlier build left is reused. Nothing interrupts it.
fn live_recheck(
    root: &Path,
    verifier: &Path,
    base: &str,
    delta: &Delta,
    resources: &LandResources,
) -> Judged {
    match nested(root, verifier, base, Some(delta), resources, &mut |_| false) {
        Ran::Ended(judged) => judged,
        Ran::Interrupted { secs, .. } | Ran::Unended { secs, .. } => Judged {
            code: exit::COULD_NOT_RUN,
            secs,
            ..Judged::default()
        },
    }
}

/// The dev set since `base`, as `verifier`, the checkout's freshly built verifier (so the
/// merged tree's tools judge it) and the landing's own image: over the cone since `base` (a
/// judgment), or, for a round, over what is left of it to judge beside `delta`'s kept
/// judgment, whose shared paths, and the files that may read its shared Markdown, it writes
/// NUL-terminated beside the run's events for the child to read ([`Round`]). Its rows are read
/// from the check-finished events it keeps and its record's end from its run-finished one.
/// While it runs, `poll` is asked as [`supervise_judgment`] asks it; a run it interrupts is no
/// verdict.
fn nested(
    root: &Path,
    verifier: &Path,
    base: &str,
    delta: Option<&Delta>,
    resources: &LandResources,
    poll: Poll<'_>,
) -> Ran {
    let began = Instant::now();
    let not_run = |why: String| {
        println!("verify land: {why}");
        Ran::Ended(Judged {
            code: exit::COULD_NOT_RUN,
            ..Judged::default()
        })
    };
    let env = crate::EnvSnapshot::capture();
    let dir = match aterm_tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => return not_run(format!("cannot make a scratch directory: {e}")),
    };
    let events = dir.path().join("verify.events");
    let written = match delta
        .map(|delta| round_files(dir.path(), delta))
        .transpose()
    {
        Ok(written) => written,
        Err(e) => return not_run(format!("cannot write the round's paths: {e}")),
    };
    let round = delta
        .zip(written.as_ref())
        .map(|(delta, (shared, readers))| Round {
            judged: &delta.judged,
            shared,
            readers: readers.as_deref(),
            mechanical: delta.mechanical,
        });
    let command = judgment_command(verifier, root, base, round, &events, resources, &env);
    let mut cmd = match command {
        Ok(cmd) => cmd,
        Err(error) => return not_run(format!("cannot own the nested verifier: {error}")),
    };
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return not_run(format!("{} did not start: {e}", verifier.display())),
    };
    let code = match supervise_judgment(&mut child, &events, poll) {
        Ok(Ending::Exited(code)) => code,
        // No verdict: nothing it wrote is read. Its wait for the judgment turn judged nothing.
        Ok(Ending::Interrupted { waited, queued }) => {
            return Ran::Interrupted {
                secs: began.elapsed().saturating_sub(waited).as_secs(),
                queued,
            };
        }
        Ok(Ending::Unended(left)) => {
            return Ran::Unended {
                secs: began.elapsed().as_secs(),
                left,
            };
        }
        // The landing took an interrupt: the run was asked to end and judged nothing.
        Ok(Ending::Stopped) => {
            return Ran::Interrupted {
                secs: began.elapsed().as_secs(),
                queued: false,
            };
        }
        Err(e) => {
            return not_run(format!(
                "{} could not be waited on: {e}",
                verifier.display()
            ));
        }
    };
    let text = std::fs::read_to_string(&events).unwrap_or_default();
    Ran::Ended(Judged::from_events(&text, code, began.elapsed().as_secs()))
}

/// What [`supervise`] reads of a running judgment and does to it: production's is [`LiveRun`].
trait Running {
    /// Wait at most `within` for the run's exit: its exit code once it exited, else `None`.
    fn wait(&mut self, within: Duration) -> std::io::Result<Option<i32>>;
    /// What the run's events have reported so far.
    fn reported(&mut self) -> Report;
    /// End the run: its group, and what that started outside it awaited ([`interrupt_judgment`]).
    /// What of that still runs.
    fn interrupt(&mut self) -> Vec<Unended>;
    /// Whether the landing took an interrupt of its own (`exec::group::taken`): none by default.
    fn stopped(&mut self) -> bool {
        false
    }
    /// Ask the run's group to end, awaiting nothing: the landing that took an interrupt has its
    /// report grace (`exec::group::REPORT_GRACE`) to record its end, and the run, tethered to it,
    /// ends with it.
    fn end_now(&mut self) {}
}

/// How a supervised judgment ended ([`supervise_judgment`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The run exited with this code.
    Exited(i32),
    /// The poll answered `true`, and the run was interrupted: it and everything it started ended.
    Interrupted {
        /// How long it had waited for the judgment turn: what its `judgment-turn` said, or, cut
        /// while still queued, since its `judgment-queued` was first read.
        waited: Duration,
        /// Whether it was still queued for the turn ([`Report::queued`]).
        queued: bool,
    },
    /// The poll answered `true`, and the run was interrupted, but these it had started outside
    /// its group still ran when the wait for them ended ([`interrupt_judgment`]).
    Unended(Vec<Unended>),
    /// The landing took an interrupt ([`Running::stopped`]): the run was asked to end, and not
    /// awaited.
    Stopped,
}

/// Wait for `run`, asking `poll` at each new stage boundary (read every [`BOUNDARY_TICK`]) and
/// at least every [`POLL`] by `now` whether to interrupt it, and telling it whether the run's own
/// answer came meanwhile ([`answered`]). Once the run reported a red it is not asked again, and
/// the run's own exit ends the supervision: its end decides, through [`decide`], as without the
/// poll. A `true` interrupts the run at once, unless it exited or
/// reported a red while the poll read main. An interrupt the landing takes ([`Running::stopped`],
/// read each [`BOUNDARY_TICK`], and ending a poll's read of main) asks the run to end and
/// returns at once, awaiting nothing ([`Ending::Stopped`]).
///
/// # Errors
/// The run could not be waited on; it may still run.
fn supervise(
    run: &mut impl Running,
    now: &mut dyn FnMut() -> Instant,
    poll: &mut dyn FnMut(Moot<'_>) -> bool,
) -> std::io::Result<Ending> {
    let mut poll_at = now() + POLL;
    let mut seen = 0;
    let mut queued_at: Option<Instant> = None;
    loop {
        let left = poll_at.saturating_duration_since(now());
        if let Some(code) = run.wait(left.min(BOUNDARY_TICK))? {
            return Ok(Ending::Exited(code));
        }
        if run.stopped() {
            run.end_now();
            return Ok(Ending::Stopped);
        }
        let report = run.reported();
        if report.unpassed {
            return own_end(run);
        }
        if report.queued && queued_at.is_none() {
            queued_at = Some(now());
        }
        if report.boundaries == seen && now() < poll_at {
            continue;
        }
        seen = report.boundaries;
        if poll(&mut || answered(run)) {
            if let Some(code) = run.wait(Duration::ZERO)? {
                return Ok(Ending::Exited(code));
            }
            if run.stopped() {
                run.end_now();
                return Ok(Ending::Stopped);
            }
            if run.reported().unpassed {
                return own_end(run);
            }
            let left = run.interrupt();
            if !left.is_empty() {
                return Ok(Ending::Unended(left));
            }
            let waited = match (report.waited_ms, queued_at) {
                (Some(ms), _) => Duration::from_millis(ms),
                (None, Some(at)) => now().saturating_duration_since(at),
                (None, None) => Duration::ZERO,
            };
            return Ok(Ending::Interrupted {
                waited,
                queued: report.queued,
            });
        }
        poll_at = now() + POLL;
    }
}

/// Whether `run`'s own answer came: it exited (or cannot be waited on), or it reported a red;
/// or the landing took an interrupt, which ends the question as well.
fn answered(run: &mut impl Running) -> bool {
    !matches!(run.wait(Duration::ZERO), Ok(None)) || run.reported().unpassed || run.stopped()
}

/// Wait for `run`'s own exit, asking nothing; an interrupt the landing takes (read each
/// [`BOUNDARY_TICK`]) asks the run to end and returns at once ([`Ending::Stopped`]).
fn own_end(run: &mut impl Running) -> std::io::Result<Ending> {
    loop {
        if let Some(code) = run.wait(BOUNDARY_TICK)? {
            return Ok(Ending::Exited(code));
        }
        if run.stopped() {
            run.end_now();
            return Ok(Ending::Stopped);
        }
    }
}

/// Wait for a judgment [`judgment_command`] started as `child`, writing its events to `events`,
/// asking `poll` at each stage boundary those report and at least every [`POLL`] whether main's
/// newest tip dooms it, until it reports a red ([`supervise`]). A `true` interrupts it
/// ([`interrupt_judgment`]), unless it exited or reported a red meanwhile: its exit is then what
/// ends it, as without the poll.
///
/// # Errors
/// The child could not be waited on: it is interrupted, so nothing it started runs on.
pub fn supervise_judgment(
    child: &mut aterm_uds::spawnfd::Child,
    events: &Path,
    poll: Poll<'_>,
) -> std::io::Result<Ending> {
    let mut run = LiveRun {
        exit: aterm_uds::deadline::ExitWait::new(child),
        child,
        events,
        tail: EventsTail::default(),
    };
    let ending = supervise(&mut run, &mut Instant::now, poll);
    if ending.is_err() {
        // The error is what the caller reads: it judges nothing after it.
        let _unended = interrupt_judgment(run.child);
    }
    ending
}

/// The child [`judgment_command`] started, and the events file it writes.
struct LiveRun<'a> {
    child: &'a mut aterm_uds::spawnfd::Child,
    exit: aterm_uds::deadline::ExitWait,
    events: &'a Path,
    tail: EventsTail,
}

impl Running for LiveRun<'_> {
    fn wait(&mut self, within: Duration) -> std::io::Result<Option<i32>> {
        let code = |status: std::process::ExitStatus| status.code().unwrap_or(exit::COULD_NOT_RUN);
        if let Some(status) = self.child.try_wait()? {
            return Ok(Some(code(status)));
        }
        self.exit.wait(within);
        Ok(self.child.try_wait()?.map(code))
    }

    fn reported(&mut self) -> Report {
        self.tail.read(self.events)
    }

    fn interrupt(&mut self) -> Vec<Unended> {
        interrupt_judgment(self.child)
    }

    fn stopped(&mut self) -> bool {
        crate::exec::group::taken().is_some()
    }

    fn end_now(&mut self) {
        if self.child.owns_unreaped()
            && let Ok(leader) = i32::try_from(self.child.id())
            && leader > 1
        {
            // SAFETY: killpg takes two integers and touches no memory of this process. The
            // group's leader is this process's unreaped child, so its id still names that group
            // alone.
            unsafe {
                libc::killpg(leader, libc::SIGTERM);
            }
        }
    }
}

/// What a run's events file reported so far ([`EventsTail::read`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Report {
    /// Its stage boundaries: each complete `stage-finished` record
    /// ([`crate::event_log::observation`]), once, and its `judgment-turn`, which starts its work:
    /// main is read at once when a queued run's turn comes.
    boundaries: usize,
    /// Whether it is queued for the judgment turn (`crate::turn`): it said `judgment-queued` and
    /// no `judgment-turn` yet, so it holds no judging work.
    queued: bool,
    /// How long it waited for the turn, ms, as its `judgment-turn` said.
    waited_ms: Option<u64>,
    /// Whether it reported a red: a complete record of what [`decide`]'s rows fold that names any
    /// word but `ok` or `skip`, a `check-finished` row ([`CheckRow::of_event`]), a decision of a
    /// `stage-finished` record or a `test-binary-finished` record. A check keeps running after
    /// one of its test binaries or stages went red, and its row comes only once all have ended.
    unpassed: bool,
}

/// A run's events file, read as it grows.
#[derive(Debug, Default)]
struct EventsTail {
    /// The file's bytes read so far.
    offset: u64,
    /// The start of a line that has not ended yet.
    partial: Vec<u8>,
    /// What its complete lines reported.
    report: Report,
}

impl EventsTail {
    /// The longest line that can hold one record.
    const LINE: usize = "verify-event: \r".len() + crate::event_log::MAX_RECORD_BYTES;

    /// What `events` has reported so far. A file not there yet, or one that does not read,
    /// reports nothing new; a line longer than any record is dropped, as no record.
    fn read(&mut self, events: &Path) -> Report {
        use std::io::{Read as _, Seek as _};
        let mut more = Vec::new();
        let read = std::fs::File::open(events).and_then(|mut file| {
            file.seek(std::io::SeekFrom::Start(self.offset))?;
            file.read_to_end(&mut more)
        });
        if read.is_err() {
            return self.report;
        }
        self.offset += u64::try_from(more.len()).unwrap_or(u64::MAX);
        self.partial.extend_from_slice(&more);
        let mut start = 0;
        while let Some(end) = self.partial[start..].iter().position(|b| *b == b'\n') {
            record(&self.partial[start..start + end], &mut self.report);
            start += end + 1;
        }
        self.partial.drain(..start);
        if self.partial.len() > Self::LINE {
            self.partial.clear();
        }
        self.report
    }
}

/// Add what one complete events line reports to `report`: a `stage-finished` record is a
/// boundary, and it, a `test-binary-finished` record or a `check-finished` row naming a word
/// that is not a pass makes it unpassed. A `judgment-queued` record makes it queued, and a
/// `judgment-turn` record ends that and is a boundary.
fn record(line: &[u8], report: &mut Report) {
    use crate::event_log::{EventKind, Observation};
    let Ok(line) = std::str::from_utf8(line) else {
        return;
    };
    let line = line.strip_suffix('\r').unwrap_or(line);
    let Some(json) = line.strip_prefix("verify-event: ") else {
        return;
    };
    let red = |word: &str| !matches!(word, "ok" | "skip");
    if json.contains("\"judgment-queued\"") || json.contains("\"judgment-turn\"") {
        match crate::event_log::observation(json) {
            Observation::Known {
                kind: EventKind::JudgmentQueued,
                ..
            } => {
                report.queued = report.waited_ms.is_none();
                return;
            }
            Observation::Known {
                kind: EventKind::JudgmentTurn,
                value,
            } => {
                report.queued = false;
                report.waited_ms = Some(value["waited_ms"].as_u64().unwrap_or(0));
                report.boundaries += 1;
                return;
            }
            _ => {}
        }
    }
    if json.contains("\"stage-finished\"") || json.contains("\"test-binary-finished\"") {
        match crate::event_log::observation(json) {
            Observation::Known {
                kind: EventKind::StageFinished,
                value,
            } => {
                report.boundaries += 1;
                report.unpassed |= value["decisions"].as_array().is_some_and(|decisions| {
                    decisions
                        .iter()
                        .any(|decision| decision["outcome"].as_str().is_some_and(red))
                });
                return;
            }
            Observation::Known {
                kind: EventKind::TestBinaryFinished,
                value,
            } => {
                report.unpassed |= value["outcome"].as_str().is_some_and(red);
                return;
            }
            _ => {}
        }
    }
    if json.contains("\"check-finished\"")
        && CheckRow::of_event(line).is_some_and(|row| red(&row.word))
    {
        report.unpassed = true;
    }
}

/// Interrupt a judgment [`judgment_command`] started, which leads its own process group, and
/// reap it. `SIGTERM` goes to the group, so its nested verifier ends its stage groups, reports
/// and dies of the signal (`exec::group::kill_on_interrupt`), and a bootstrap still compiling
/// ends with its compilers. Once the leader exited, or after [`INTERRUPT_GRACE`], what is left of
/// the group is killed and the leader reaped ([`aterm_uds::deadline::settle_child`]). Nothing
/// outside the group is signalled. What its members had started outside it ([`started_outside`]:
/// read before the signal, and again while the leader ends, so what they start meanwhile joins)
/// ends by its own parent-death watch, a step at a time: each stage group once its verifier is
/// gone, and its children once their parents are. This returns once all of that is gone, and
/// what those start meanwhile with it ([`await_ended`]): what the run held (its capacity claims,
/// its build's locks, the build directory its compilers write) is released as its holders end,
/// so the re-judge after it finds them free. Where [`INTERRUPT_GRACE`] passes with none of what
/// was read ending, what still runs is named on stdout and awaited [`UNENDED_BOUND`] more in
/// all; what runs even then is returned, and the landing judges nothing after it
/// ([`Ran::Unended`]). The interrupted run is no verdict.
pub fn interrupt_judgment(child: &mut aterm_uds::spawnfd::Child) -> Vec<Unended> {
    interrupt(child, INTERRUPT_GRACE, UNENDED_BOUND)
}

/// [`interrupt_judgment`], awaiting what the group started outside it until `outside` passed
/// with none of it ending, in place of [`INTERRUPT_GRACE`], and no longer: a test passes its hang
/// detector, so that only a hang fails it, by what this returns.
#[cfg(any(test, debug_assertions))]
pub fn interrupt_judgment_awaiting(
    child: &mut aterm_uds::spawnfd::Child,
    outside: Duration,
) -> Vec<Unended> {
    interrupt(child, outside, Duration::ZERO)
}

fn interrupt(
    child: &mut aterm_uds::spawnfd::Child,
    grace: Duration,
    bound: Duration,
) -> Vec<Unended> {
    let mut group = None;
    let mut outside = Vec::new();
    if child.owns_unreaped()
        && let Ok(leader) = i32::try_from(child.id())
        && leader > 1
    {
        outside = started_outside(Some(leader.unsigned_abs()), &[]).unwrap_or_default();
        // SAFETY: killpg takes two integers and touches no memory of this process. The group's
        // leader is this process's unreaped child, so its id still names that group alone.
        unsafe {
            libc::killpg(leader, libc::SIGTERM);
        }
        group = Some(leader.unsigned_abs());
    }
    let until = Instant::now() + INTERRUPT_GRACE;
    let mut exit = aterm_uds::deadline::ExitWait::new(child);
    while matches!(child.exited(), Ok(false)) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        exit.wait(left.min(OUTSIDE_TICK));
        // The leader is unreaped, so the group's id still names it: what its members, and what
        // was read outside it, start meanwhile joins the wait. A table that does not read keeps
        // what was read.
        if group.is_some()
            && let Some(now) = started_outside(group, &outside)
        {
            outside = now;
        }
    }
    aterm_uds::deadline::settle_child(child);
    await_ended(outside, grace, bound, &mut LiveOutside)
        .iter()
        .map(Started::unended)
        .collect()
}

/// What [`await_ended`] reads and does: production's is [`LiveOutside`].
trait Outside {
    /// The time.
    fn now(&mut self) -> Instant;
    /// Those of `known` that still run, and every process they started, and theirs in turn.
    fn left(&mut self, known: &[Started]) -> Vec<Started>;
    /// Wait one [`OUTSIDE_TICK`].
    fn nap(&mut self);
    /// Say that `left` still runs and is awaited at most `bound` more.
    fn named(&mut self, left: &[Started], bound: Duration);
}

/// Wait until `outside`, what an interrupted judgment started outside its group, and what that
/// starts meanwhile, is gone: nothing, then. Each end of one of `outside` renews `grace`: the
/// chain of parent-death watches ends it a step at a time, and `outside` only shrinks, so the
/// renewals end. Once `grace` passed with none of `outside` ending, what still runs is named
/// ([`Outside::named`]) and awaited `bound` more in all, which nothing renews; what still runs
/// then is the answer. A zero `bound` names nothing and answers at once.
fn await_ended(
    outside: Vec<Started>,
    grace: Duration,
    bound: Duration,
    world: &mut impl Outside,
) -> Vec<Started> {
    let mut first = outside.clone();
    let mut left = outside;
    let mut until = world.now() + grace;
    let mut last = None;
    loop {
        left = world.left(&left);
        if left.is_empty() {
            return left;
        }
        let now = world.now();
        let before = first.len();
        first.retain(|started| left.contains(started));
        match last {
            Some(last) if now >= last => return left,
            Some(_) => {}
            None => {
                if first.len() < before {
                    until = now + grace;
                }
                if now >= until {
                    if bound.is_zero() {
                        return left;
                    }
                    world.named(&left, bound);
                    last = Some(now + bound);
                }
            }
        }
        world.nap();
    }
}

/// The process table, the clock and stdout ([`Outside`]).
struct LiveOutside;

impl Outside for LiveOutside {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    /// A table that does not read keeps those of `known` that still run.
    fn left(&mut self, known: &[Started]) -> Vec<Started> {
        started_outside(None, known)
            .unwrap_or_else(|| known.iter().copied().filter(Started::running).collect())
    }

    fn nap(&mut self) {
        std::thread::sleep(OUTSIDE_TICK);
    }

    fn named(&mut self, left: &[Started], bound: Duration) {
        let left: Vec<Unended> = left.iter().map(Started::unended).collect();
        println!(
            "verify land: the interrupted judgment left {} still running: {}; waiting at most \
             {}s more for {} to end",
            processes(left.len()),
            unended_words(&left),
            bound.as_secs(),
            if left.len() == 1 { "it" } else { "them" },
        );
    }
}

/// How often [`interrupt_judgment`] reads what the group started outside it.
const OUTSIDE_TICK: Duration = Duration::from_millis(100);

/// A process by its pid and birth, which an exec in its place keeps: on Apple an exec moves its
/// pid version (a tethered stage's exec of its program does), so the whole identity is not what
/// names it ([`aterm_uds::tether::Watched::token`] drops the version too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Started {
    pid: i32,
    birth: aterm_uds::process::Birth,
}

impl Started {
    const fn of(row: &aterm_uds::process::Lineage) -> Self {
        Self {
            pid: row.identity.pid,
            birth: row.identity.birth,
        }
    }

    /// Whether it still runs: its pid reads its birth ([`aterm_uds::runs::LiveKernel`]).
    fn running(&self) -> bool {
        use aterm_uds::runs::Kernel as _;
        aterm_uds::runs::LiveKernel.birth(self.pid) == Some(self.birth)
    }

    /// It by its pid and its executable's name, as the kernel reads that now.
    fn unended(&self) -> Unended {
        use aterm_uds::runs::Kernel as _;
        Unended {
            pid: self.pid,
            word: aterm_uds::runs::LiveKernel
                .exe(self.pid)
                .as_deref()
                .and_then(aterm_uds::runs::exe_word)
                .unwrap_or_else(|| "?".to_owned()),
        }
    }
}

/// [`beyond`] by a read of this user's process table ([`aterm_uds::runs::LiveKernel`]), or
/// `None` where it does not read.
fn started_outside(group: Option<u32>, known: &[Started]) -> Option<Vec<Started>> {
    use aterm_uds::runs::Kernel as _;
    let kernel = aterm_uds::runs::LiveKernel;
    let rows: Vec<_> = kernel
        .uid_pids()?
        .into_iter()
        .filter_map(|pid| kernel.lineage(pid))
        .collect();
    Some(beyond(&rows, group, known))
}

/// What a signal to `group` does not reach, by one read of the process table (`rows`): every
/// process outside `group` that one of its members started, or one of `known` that still runs (by
/// its pid and birth), and theirs in turn, with `known`'s that still run. No `group` (its leader
/// was reaped, so its id names it no more) has no members: `known`'s that still run and theirs.
fn beyond(
    rows: &[aterm_uds::process::Lineage],
    group: Option<u32>,
    known: &[Started],
) -> Vec<Started> {
    let mut walked = BTreeSet::new();
    let mut parents = Vec::new();
    let mut found = Vec::new();
    for row in rows {
        let member = group == Some(row.pgid);
        if member || known.contains(&Started::of(row)) {
            walked.insert(row.identity.pid);
            parents.push(row.identity.pid);
            if !member {
                found.push(Started::of(row));
            }
        }
    }
    while let Some(parent) = parents.pop() {
        for row in rows
            .iter()
            .filter(|row| row.parent == parent.unsigned_abs())
        {
            if walked.insert(row.identity.pid) {
                parents.push(row.identity.pid);
                found.push(Started::of(row));
            }
        }
    }
    found
}

/// The files a round's child reads its delta from ([`Round`]), written into `dir`: `delta`'s
/// shared paths, and, where any file may read its shared Markdown, those files
/// ([`Delta::readers`]), each path ended by a NUL.
///
/// # Errors
/// A file could not be written.
fn round_files(dir: &Path, delta: &Delta) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
    let write = |name: &str, paths: &[String]| {
        let file = dir.join(name);
        let mut bytes = Vec::new();
        for path in paths {
            bytes.extend_from_slice(path.as_bytes());
            bytes.push(0);
        }
        std::fs::write(&file, bytes).map(|()| file)
    };
    let shared = write("verify.shared", &delta.shared)?;
    let readers = if delta.readers.is_empty() {
        None
    } else {
        Some(write("verify.readers", &delta.readers)?)
    };
    Ok((shared, readers))
}

/// A round's selection as its child's words name it: `--judged <commit> --shared <file>`
/// ([`crate::cli::Args::judged`]), with `--readers <file>` where a file may read its shared
/// Markdown, or `--by-hand` for a merge that is not the landing's own.
#[derive(Clone, Copy, Debug)]
pub struct Round<'a> {
    /// The judged commit ([`Delta::judged`]).
    pub judged: &'a str,
    /// The file that holds the shared paths ([`Delta::shared`]), each ended by a NUL.
    pub shared: &'a Path,
    /// The file that holds the files that may read the shared Markdown ([`Delta::readers`]),
    /// each ended by a NUL; none where no file may.
    pub readers: Option<&'a Path>,
    /// Whether the merge is the landing's own ([`Delta::mechanical`]): where it is not, the
    /// child is told so ([`crate::cli::BY_HAND`]) and judges no shared path's member alone.
    pub mechanical: bool,
}

/// Run the checkout's freshly built verifier directly, owned by the calling land process, on
/// the dev set since `base`: over the cone since it (a judgment), or, with `round`, over what
/// is left of that cone to judge beside the round's kept judgment (`crate::changed::resolve_round`;
/// [`verifier_command`] with a judgment's words). Nothing is carried between judgments. Its
/// events go to `events` alone
/// ([`crate::cli::EVENTS_FILE_ONLY`]): the landing reads them there, and its console carries
/// what a person reads. Its first red ends it (`--fail-fast`).
/// Its words are composed and parsed by one build: a landing whose image is not the checkout's
/// build hands itself over to that build first ([`handoff`]), so these flags are the landing's
/// own, internal, and may change with any main.
/// # Errors
/// This process's executable path could not be read.
pub fn judgment_command(
    verifier: &Path,
    root: &Path,
    base: &str,
    round: Option<Round<'_>>,
    events: &Path,
    resources: &LandResources,
    env: &crate::EnvSnapshot,
) -> std::io::Result<Command> {
    let mut cmd = verifier_command(verifier, root, env)?;
    cmd.args(["--base", base]);
    if let Some(round) = round {
        cmd.args([crate::cli::JUDGED, round.judged, crate::cli::SHARED])
            .arg(round.shared);
        if let Some(readers) = round.readers {
            cmd.arg(crate::cli::READERS).arg(readers);
        }
        if !round.mechanical {
            cmd.arg(crate::cli::BY_HAND);
        }
    }
    cmd.arg("--events")
        .arg(events)
        .arg(crate::cli::EVENTS_FILE_ONLY)
        .args([aterm_capacity::Admit::FLAG, resources.admit.word()])
        .arg(crate::cli::QUEUED_SINCE)
        .arg(queued_since().to_string())
        .arg("--fail-fast");
    if let Some(timeout) = resources.timeout.as_deref() {
        cmd.args(["--timeout", timeout]);
    }
    if let Some(cores) = resources.test_cores {
        cmd.args(["--test-cores", &cores.to_string()]);
    }
    Ok(cmd)
}

/// When this landing was first queued, unix ms: this process's own start, which the in-place
/// exec of a handover to the merged tree's verifier keeps (`--queued-since`). Every judgment and
/// round of one landing so keeps the landing's place in the judgment turn's queue
/// (`crate::turn`), however often main makes it judge again. Linux keeps no wall start record:
/// there it is the boot plus the start's clock ticks, in whole seconds
/// (`aterm_uds::process::read_process_start_s`), the same at every call. Where the kernel's start
/// record is unread, now.
fn queued_since() -> u64 {
    let pid = i32::try_from(std::process::id()).ok();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let born = pid
        .and_then(aterm_uds::process::read_process_start_s)
        .map(|seconds| seconds.saturating_mul(1000));
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let born = pid
        .and_then(aterm_uds::process::read_process_birth)
        .map(|birth| {
            birth
                .seconds
                .saturating_mul(1000)
                .saturating_add(birth.microseconds / 1000)
        });
    born.unwrap_or_else(|| aterm_time::unix_millis().unwrap_or(0))
}

/// The checkout's freshly built verifier as a judgment runs it, before its words: in the
/// checkout's root, leading its own process group, tethered to the calling land process.
/// The explicit trampoline uses the caller's current executable, whose `main` must serve
/// [`aterm_uds::tether::entry`]. It does not depend on the image recorded before a rebuild.
/// Linux runs its loaded image through `/proc/self/exe`, which a replaced pathname cannot change.
/// # Errors
/// This process's executable path could not be read.
pub fn verifier_command(
    verifier: &Path,
    root: &Path,
    env: &crate::EnvSnapshot,
) -> std::io::Result<Command> {
    #[cfg(unix)]
    let mut cmd = {
        #[cfg(target_os = "linux")]
        let image = std::path::PathBuf::from("/proc/self/exe");
        #[cfg(not(target_os = "linux"))]
        let image = std::env::current_exe()?;
        let mut cmd = aterm_uds::spawnfd::command(image);
        cmd.arg0(aterm_uds::tether::NAME)
            .arg(aterm_uds::tether::VERB)
            .arg(std::process::id().to_string())
            .arg(verifier)
            .arg(verifier);
        cmd
    };
    #[cfg(not(unix))]
    let mut cmd = aterm_uds::spawnfd::command(verifier);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.current_dir(root);
    crate::native_verifier::configure_runtime(&mut cmd, root, env);
    Ok(cmd)
}

/// The hang detector on one reach read ([`live_reach`]): its graph reads are `targo tree`
/// calls and Git reads with no bound of their own, and a poll that waited on one would hold a
/// landing whose judgment has ended.
const REACH: Duration = Duration::from_secs(120);

/// [`read_reach`] on a thread of its own, awaited at most [`REACH`]: a read that does not end
/// (a toolchain or package-cache lock another process holds) is left behind and answers
/// [`Reach::Unread`], said once here, which cuts no judgment.
fn live_reach(root: &Path, from: &str, paths: Option<&[String]>) -> Reach {
    let (done, read) = std::sync::mpsc::channel();
    let asked = (
        root.to_owned(),
        from.to_owned(),
        paths.map(<[String]>::to_vec),
    );
    let started = std::thread::Builder::new()
        .name("land-reach".to_owned())
        .spawn(move || {
            let (root, from, paths) = asked;
            let _ = done.send(read_reach(&root, &from, paths.as_deref()));
        });
    let until = Instant::now() + REACH;
    let found = match started {
        Err(error) => Reach::Unread(format!("its read did not start: {error}")),
        // Awaited a tick at a time, so an interrupt the landing took ends the wait.
        Ok(_) => loop {
            let left = until.saturating_duration_since(Instant::now());
            match read.recv_timeout(left.min(aterm_uds::deadline::STOP_TICK)) {
                Ok(found) => break found,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if taken() => {
                    break Reach::Unread("the landing took an interrupt".to_owned());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if left.is_zero() => {
                    break Reach::Unread(format!(
                        "its read did not end within {}s",
                        REACH.as_secs()
                    ));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break Reach::Unread("its read ended with no answer".to_owned());
                }
            }
        },
    };
    if let Reach::Unread(why) = &found {
        println!(
            "verify land: what {} since {from} reaches was not read ({why}): it interrupts no \
             judgment",
            if paths.is_some() {
                "main's change"
            } else {
                "the branch's change"
            }
        );
    }
    found
}

/// What `paths`, as changed since `from`, reach in the checkout at `root`, or, with none, what
/// the change since `from` does: the crates of the cone [`crate::changed`] selects (seeds,
/// dependents, readers and namers), with no member judged alone for a path list. A toolchain
/// that does not resolve leaves it unread.
fn read_reach(root: &Path, from: &str, paths: Option<&[String]>) -> Reach {
    let env = crate::EnvSnapshot::capture();
    let tools = match crate::preflight(&env, root) {
        Ok(tools) => tools,
        Err(why) => return Reach::Unread(format!("the toolchain did not resolve: {why}")),
    };
    let path_env = tools.path_with_stage2_first(&env.path);
    let selection = match paths {
        None => crate::changed::resolve(root, &tools, &path_env, from),
        Some(_) if !tools.have_targo() => {
            return Reach::Unread(
                "targo is absent, so the dependency graph cannot be read".to_owned(),
            );
        }
        Some(paths) => {
            crate::changed::resolve_paths(root, &tools, &path_env, from, paths.to_vec(), None)
        }
    };
    match selection {
        crate::changed::Selection::Widened(why) => Reach::Everything(why),
        crate::changed::Selection::Narrowed { crates, .. } => Reach::Crates(
            crates
                .iter()
                .filter_map(|spec| crate::scope::spec_name(spec))
                .map(str::to_owned)
                .collect(),
        ),
    }
}

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;
