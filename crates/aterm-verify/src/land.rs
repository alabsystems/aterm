// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified verify land` (design B10, `docs/DESIGN-fast-build-release-2026-11-05.md`
//! §4.6): land this checkout's branch on main, judged.
//!
//! 1. Fetch main and merge it (M1), stopping on a conflict. The same rule owns each re-judge's
//!    merge (step 4). A change's changelog entry is its own file under `changes/`, so two
//!    branches' entries are disjoint paths and need no merge of their own.
//! 2. Run the dev set over the cone since M1 (`verify --base M1`, B7's export scan included),
//!    on the committed checkout: one that changes during the run pushes nothing. While it runs,
//!    main is read at each stage boundary the run's events report and at least every [`POLL`]
//!    (`docs/DESIGN-landing-economy.md` §2.0). The first tip step 4's rule does not compose
//!    interrupts the run, ending its group ([`interrupt_judgment`]): the interrupted run is no
//!    verdict, and step 4's merge and re-judge follow at once, counted toward [`REJUDGES`]. A
//!    tip that composes (by either rule of step 4) changes nothing. Once the run reports a red (a check, a stage or a test
//!    binary with no pass), even one step 3 may yet inherit, main is not read again: its end is
//!    step 3's to decide, as without the poll.
//! 3. A red main's lane verdict on the merged main commit names (B8's journal,
//!    [`crate::judged`]; M1, or on a re-judge the moved main it merged; read once the run ends,
//!    so a verdict the lane gave meanwhile counts), in a crate outside the branch's own
//!    ([`Branch`]), is INHERITED: named and not counted. Any other red, and a run
//!    whose record is not whole, exits non-zero, and nothing is pushed.
//! 4. Push (a push git reports failed whose landing main holds landed). Where main moved to M2
//!    meanwhile, compose the pass onto M2 when AGENTS.md's rule holds: a clean merge and
//!    disjoint endpoint changes after discarding only shared paths with identical mode,
//!    type and object (or all absent) in checked, new main and merge, including parent trees.
//!    Unavailable entry evidence keeps the original overlap. A sole remaining shared
//!    `Cargo.lock` may instead carry an isolated local product-version move
//!    ([`version::independent_lock`]). The four immutable trees retain every other lock
//!    byte and the source-proven binary-leaf identity boundary. That
//!    is seconds of plumbing, so a main that moves again is composed onto again at once
//!    ([`COMPOSES`] in a row at most, rechecks included). That count is the push's bound: no
//!    deadline spans the landings of one judgment, and each remote step runs under its own hang
//!    detector ([`NETWORK`]; a write under [`WRITE`]). A remote step that fails or times out
//!    having sent nothing (a read of main, a push agh says it did not send) is made again after
//!    a doubling pause, [`RETRIES`] in a row at most; a push whose outcome is unknown is never
//!    made again, and a main that holds its landing took it. Where the only shared paths are
//!    Markdown no build, include or test reads (root `*.md`, `docs/**/*.md`,
//!    a member's `SPECS.md`: [`aterm_census::no_reach`], regular files on both sides and in the
//!    merge, named in no Rust code: [`Git::markdown_shared`]), merge M2 and recheck the merge
//!    with only the source readers ([`RECHECKS`]), as the merge's own freshly built verifier,
//!    over the cone since M2. A recheck that passes is recorded as `Aterm-Rechecked` and the
//!    merge is pushed (composed onto again as any judgment is); any red it reports counts, and
//!    a merge whose tree is not the one the rule tested is judged in full. Where neither rule
//!    holds, merge M2 and judge the branch's
//!    delta from M2 against its new dependencies (`verify --base M2`); after [`REJUDGES`]
//!    re-judges it stops with the branch merged and judged (or its judgment interrupted), and
//!    says so. Semantic breaks across disjoint files are B8's.
//! 5. The landing merge states what ran ([`aterm_types::land_trailer`]), an interrupted judgment
//!    as interrupted. A branch whose head already merges the main it lands on is that merge,
//!    restated with the trailers.
//! 6. A judgment that passed is kept in the worktree ([`KEPT`]) until its landing is on main, so
//!    no ending between the pass and the push loses it: the next run whose HEAD is the judged
//!    commit, holding the judged tree, starts at step 4 with that receipt, and judges only what
//!    step 4's rule says it must.
//!
//! Each judgment and each recheck runs as the verifier built from the tree it judges: the argv
//! its child gets, that child's parse of it, the reading of its record and every rule after it
//! are one build's. Before each, the landing builds the checkout's verifier; where that build is
//! not this process's own image (sha256, read at the start, before any build), the landing
//! writes a handoff and replaces itself in place with that build as `land --resume <file>`
//! ([`handoff`]). The receiver checks that the checkout still stands at the step's commit,
//! parses the person's `land` words again and resumes the same step with the receipt, the
//! re-judge and composition counts, and the branch's crates or the cone they are read over.
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
use aterm_types::land_trailer::{Checked, Interrupted, Rechecked, Row, Stated};
use aterm_uds::spawnfd::Command;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "land_engine.rs"]
mod engine;
#[path = "land_handoff.rs"]
pub mod handoff;
#[path = "land_version.rs"]
mod version;

pub use handoff::{Handoff, Image};

/// The remote main is fetched from and pushed to.
pub const REMOTE: &str = "origin";
/// The branch the landing moves.
pub const MAIN: &str = "main";
/// How many times a moved main is merged and judged again before the tool stops. A judgment a
/// main tip interrupted counts as the re-judge that follows it.
pub const REJUDGES: usize = 3;
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
/// How many times one step may be handed to another image ([`handoff`]): once by the image
/// that ran the steps before, and once more by that receiver, built from the step's tree by
/// the sender's build recipe, whose own build by the tree's recipe may differ.
pub const HOPS: usize = 2;
/// How many composition and pre-send obstruction races are allowed before the tool stops.
/// A different writer or intent never renews this bound.
pub const COMPOSES: usize = 10;
/// The exit of a landing the tool stopped while main kept moving: the branch is merged and
/// judged, not pushed (`EX_TEMPFAIL`, as a yielding `verify watch --sample`).
pub const STOPPED: i32 = 75;
/// How many remote failures in a row that sent nothing the push of a passed judgment makes
/// again before the landing stops with its judgment kept: reads of main that failed or timed
/// out, and, counted apart, pushes agh says it did not send
/// ([`crate::remote::Pushed::Unsent`]).
pub const RETRIES: usize = 5;
/// The pause before a failed remote step is made again; each further failure in a row doubles it.
pub const RETRY_PAUSE: Duration = Duration::from_secs(2);
/// The hang detector on one write of main ([`Wire::write`]): the longest credential grant agh
/// issues ([`agh::git::GRANT_MAX`]), beyond agh's own wall bound on each of its git children
/// ([`agh::git::GIT_BUDGET`]). agh opens its write's durable intent before its re-verification
/// of the pushed history and settles it when it ends, so agh ends its own write first: a write
/// this tool killed would leave that intent open and refuse every later write of the repository.
const WRITE: Duration = agh::git::GRANT_MAX;
const _: () = assert!(WRITE.as_secs() > agh::git::GIT_BUDGET.as_secs());
/// Where a passed judgment is kept until its landing is on main ([`Git::keep`]): a ref of this
/// worktree alone, which no fetch or push moves.
pub const KEPT: &str = "refs/worktree/aterm/land-judged";

/// The checks a recheck runs: the source readers, the only checks that read no-reach Markdown
/// (`aterm_census::no_reach`).
pub const RECHECKS: [&str; 3] = ["fmt", "census", "export-scan"];
/// The hang detector on a local git read or write.
const LOCAL: Duration = Duration::from_secs(60);

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
/// The hang detector on a git step that reaches the network or checks out a merge.
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

/// The branch's own crates: a red in one of them is the branch's, whatever main's lane says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Branch {
    /// The cone's seeds, by name: the crates the change itself touched, and those whose shipped
    /// code spells another crate's changed path.
    Crates(BTreeSet<String>),
    /// The change reaches every crate (a widened cone: a shared input changed), or its crates
    /// were not read; the reason.
    Everything(String),
}

impl Branch {
    /// Whether a red in `krate` is the branch's.
    #[must_use]
    pub fn touches(&self, krate: &str) -> bool {
        match self {
            Self::Crates(crates) => crates.contains(krate),
            Self::Everything(_) => true,
        }
    }
}

/// Main's lane verdict on a main commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lane {
    /// Judged red: every red item its red rows name.
    Red(BTreeSet<String>),
    /// Judged green.
    Pass,
    /// No decided verdict (none, a live or lapsed claim, could not run, unreadable): the reason.
    Unjudged(String),
}

/// What a run's reds come to under main's lane verdict and the branch's crates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// The checks whose reds count (and a failed run that named no failed check).
    pub counted: Vec<String>,
    /// The red items inherited from main, each once.
    pub inherited: Vec<String>,
    /// The checks that did not run, and a run that did not finish or whose record is not whole.
    pub not_run: Vec<String>,
}

/// The crate a red item names: the one package of a test finding (`-p <spec> <flag> -- <name>`,
/// `findings::test_findings`); `None` for any other item (a row label, a lint's, one naming
/// several packages).
#[must_use]
pub fn crate_of(id: &str) -> Option<&str> {
    let (spec, name) = id.split_once(" -- ")?;
    let mut words = spec.split_whitespace();
    if name.is_empty() || words.next()? != "-p" {
        return None;
    }
    let krate = words.next().and_then(crate::scope::spec_name)?;
    (!words.any(|word| word == "-p")).then_some(krate)
}

/// THE INHERITANCE RULE: a failed check's reds are inherited only when the run's record is whole
/// ([`Judged::whole`]), it named every one of them (no finding past the event's cap), main's lane
/// judged red on the merged main commit and names each one, and each is a test of a crate
/// outside the branch's own. Any other failed check counts; anything that did not run, and a
/// record not whole, is not a pass.
#[must_use]
pub fn decide(judged: &Judged, lane: &Lane, branch: &Branch) -> Decision {
    let mut decision = Decision::default();
    let whole = judged.whole();
    let named = match lane {
        Lane::Red(items) => Some(items),
        Lane::Pass | Lane::Unjudged(_) => None,
    };
    for row in &judged.checks {
        match row.word.as_str() {
            "ok" | "skip" => {}
            "FAIL" => {
                let all_named =
                    !row.findings.is_empty() && row.findings_total == row.findings.len();
                let inherited = whole
                    && all_named
                    && named.is_some_and(|named| {
                        row.findings.iter().all(|id| {
                            named.contains(id) && crate_of(id).is_some_and(|c| !branch.touches(c))
                        })
                    });
                if inherited {
                    for id in &row.findings {
                        if !decision.inherited.contains(id) {
                            decision.inherited.push(id.clone());
                        }
                    }
                } else {
                    decision.counted.push(row.check.clone());
                }
            }
            _ => decision.not_run.push(row.check.clone()),
        }
    }
    if !whole {
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
        exit::FAILED if decision.counted.is_empty() && decision.inherited.is_empty() => {
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
    decision
}

/// Reads whether a poll's question became moot ([`Poll`]): the run it asks about exited, or
/// reported a red, so its own end decides.
pub type Moot<'m> = aterm_land::Moot<'m>;

/// Asked while a judgment runs whether main's newest tip dooms it: `true` interrupts the run. Its
/// argument reads whether the question became moot meanwhile ([`Moot`]); its read of main ends
/// once it does.
pub type Poll<'p> = &'p mut dyn FnMut(Moot<'_>) -> bool;

/// How a judgment's run ([`Seams::judge`]) came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    /// It ended by itself: what it judged.
    Ended(Judged),
    /// A poll's `true` interrupted it ([`interrupt_judgment`]): no verdict, and nothing it wrote
    /// is read.
    Interrupted {
        /// Its wall seconds until it ended.
        secs: u64,
    },
}

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
    /// image), over the cone since a base commit. While it runs it asks its fourth argument
    /// whether main's newest tip dooms it, at each stage boundary its run reports and at least
    /// every [`POLL`], until the run reports a red (`FAIL`, `NOT RUN`: a check, a stage or a
    /// test binary): its own end then decides, as without the poll, an inherited red included.
    /// A `true` interrupts the run ([`interrupt_judgment`], [`Ran::Interrupted`]) unless it
    /// ended or reported such a red meanwhile.
    pub judge: &'a mut dyn FnMut(&Path, &Path, &str, Poll<'_>) -> Ran,
    /// The branch's own crates: the seeds of the cone since the merged main commit.
    pub branch: &'a mut dyn FnMut(&Path, &str) -> Branch,
    /// Main's lane verdict on a main commit, read once each judgment ends.
    pub lane: &'a mut dyn FnMut(&Path, &str) -> Lane,
    /// Rerun only the source readers ([`RECHECKS`]) on the checkout as it stands, as the
    /// verifier at the second argument (the checkout's own fresh build, the landing's own
    /// image), over the cone since a base commit: the recheck of a Markdown compose
    /// ([`Composition::Rechecks`]). Nothing interrupts it.
    pub recheck: &'a mut dyn FnMut(&Path, &Path, &str) -> Judged,
    /// Hand the landing to an image (the first argument) with the handoff file it wrote (the
    /// second), in the checkout (the third) ([`handoff::hand_over`]): production's replaces
    /// this process and returns only when that failed, with the exit.
    pub hand_over: &'a mut dyn FnMut(&Image, &Path, &Path) -> i32,
    /// Say a line to the person.
    pub say: &'a mut dyn FnMut(&str),
    /// One remote step of the push phase (each push, and each read of main once a judgment
    /// passed): [`crate::remote::run`] in production.
    pub(crate) remote: &'a RemoteStep<'a>,
    /// Wait out the pause before a failed remote step is made again ([`RETRY_PAUSE`]), and each
    /// tick of a wait on another writer's open intent.
    pub pause: &'a dyn Fn(Duration),
}

/// How the push phase runs one remote step: a git or agh command for the checkout at the path,
/// its git made by the maker, ended as hung at the instant ([`crate::remote::run`]'s shape).
pub(crate) type RemoteStep<'a> = dyn Fn(
        &Path,
        &dyn Fn(&Path) -> Command,
        crate::remote::Tool,
        &[std::ffi::OsString],
        Instant,
    ) -> Result<std::process::Output, String>
    + 'a;

/// Who lands: the person's `land` words, which a handoff carries for the receiver to parse
/// again, and this image's sha256, read before any build.
pub struct Own<'a> {
    /// The person's `land` words.
    pub words: &'a [String],
    /// This process's own image's digest.
    pub image: &'a str,
}

/// What a refused handoff leaves the person to do.
const RERUN: &str = "nothing was pushed. Fix: run `targo --unverified verify land` again: the \
                     checkout holds the merge, committed";
/// What a handoff whose checkout left its commit before the resume leaves the person to do.
const RERUN_MOVED: &str = "nothing was pushed. Fix: run `targo --unverified verify land` again \
                           once the checkout holds what is to land, committed";

/// Land the checkout at `root` ([`Seams`]) as `own`: the exit code ([`exit`], or [`STOPPED`]).
pub fn land(root: &Path, own: &Own<'_>, seams: &mut Seams<'_>) -> i32 {
    let git = Git {
        root,
        make: seams.git,
    };
    match drive(&git, own, seams) {
        Ok(code) => code,
        Err(why) => {
            (seams.say)(&format!("verify land: COULD NOT RUN — {why}"));
            exit::COULD_NOT_RUN
        }
    }
}

/// Resume, as `own`, the landing of the checkout at `root` another build of this verifier
/// handed over ([`handoff`]): refused, before anything runs, where the handoff names another
/// image or checkout, or a base or cone its commit does not hold. The exit code, as
/// [`land`]'s.
pub fn resume(root: &Path, own: &Own<'_>, handoff: Handoff, seams: &mut Seams<'_>) -> i32 {
    let git = Git {
        root,
        make: seams.git,
    };
    match resume_drive(&git, own, handoff, seams) {
        Ok(code) => code,
        Err(why) => {
            (seams.say)(&format!("verify land: COULD NOT RUN — {why}"));
            exit::COULD_NOT_RUN
        }
    }
}

fn resume_drive(
    git: &Git<'_>,
    own: &Own<'_>,
    handoff: Handoff,
    seams: &mut Seams<'_>,
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
    engine::resume(git, seams, own, &subject, handoff)
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

fn drive(git: &Git<'_>, own: &Own<'_>, seams: &mut Seams<'_>) -> Result<i32, String> {
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
    let start = git.head()?;
    if git.is_ancestor(&start, &merged)? {
        git.forget();
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
            say(
                seams,
                format!("verify land: the kept judgment is not resumed: {why}"),
            );
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
            if !git.is_ancestor(&merged, &start)? {
                match git.merge(&merged, &subject)? {
                    Merge::Clean => {}
                    Merge::Conflict(paths) => return Ok(conflicted(seams, &merged, &paths)),
                }
            }
            (git.head()?, merged, None)
        }
    };
    let revision = aterm_land::Revision {
        at: judged_at,
        base: merged,
    };
    engine::drive(git, seams, own, &subject, revision, resumed, moved)
}

/// The branch's own crates over the cone since main commit `base` ([`Seams::branch`]), said.
fn own_crates(git: &Git<'_>, seams: &mut Seams<'_>, base: &str) -> Branch {
    let branch = (seams.branch)(git.root, base);
    (seams.say)(&match &branch {
        Branch::Crates(crates) => format!(
            "verify land: the branch's crates: {}",
            crates.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        Branch::Everything(why) => {
            format!("verify land: the branch reaches every crate ({why}): no red is inherited")
        }
    });
    branch
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

/// How a recheck ([`recheck`]) came out.
enum Rechecking {
    /// It passed: its recheck is in the receipt.
    Passed,
    /// The landing ends with this exit.
    Exit(i32),
    /// It did not run: the merge's own verifier is this image, which the landing goes on as.
    Elsewhere(Image),
}

/// What the landing holds between its steps.
struct Held<'o> {
    /// The branch's own crates, read before the first judgment that runs, as the image that
    /// runs it: a landing resumed from a kept judgment that judges nothing reads no cone.
    branch: Option<Branch>,
    /// The main commit the landing's first judgment merged: the cone the branch's crates are
    /// read over.
    cone: String,
    /// The receipt so far.
    stated: Stated,
    /// This image's digest.
    own: &'o str,
    /// The handoffs the step about to run has taken: a resumed step's [`Handoff::hop`], 0 for
    /// one this image came to itself. A step that yields hands over as hop one more.
    hop: usize,
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
    let image = match (seams.verifier)(git.root) {
        Ok(image) => image,
        Err(why) => {
            (seams.say)(&format!(
                "verify land: COULD NOT RUN — {tree}'s verifier did not build: {why}; nothing was \
                 pushed"
            ));
            return Ok(Built::Exit(exit::COULD_NOT_RUN));
        }
    };
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

/// A judgment an earlier run kept ([`Git::keep`]) that holds for this checkout ([`Git::kept`]).
struct Kept {
    /// The judged tree, which HEAD holds.
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
    ) -> Self {
        Self {
            git,
            step,
            pause,
            say,
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
        )
    }

    /// One step of a write of main through the seam: ended as hung [`WRITE`] after it starts,
    /// which agh's own bounds come before.
    fn write(
        &self,
        tool: crate::remote::Tool,
        args: &[std::ffi::OsString],
    ) -> Result<std::process::Output, String> {
        (self.step)(
            self.git.root,
            self.git.make,
            tool,
            args,
            Instant::now() + WRITE,
        )
    }

    /// Allow `what`, which failed for `why` having sent nothing, to be made again: counted in
    /// `failed`, said, and its pause waited out ([`RETRY_PAUSE`], doubled by each failure before
    /// it in a row).
    ///
    /// # Errors
    /// [`RETRIES`] such failures already came in a row: `why`, and that it stopped.
    fn again(&self, failed: &std::cell::Cell<usize>, what: &str, why: &str) -> Result<(), String> {
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
        let writer = crate::remote::Remote::open(self.git.root, REMOTE, &write)?;
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
                let pushed = writer
                    .push(&format!("refs/heads/{MAIN}"), Some(onto), landing)
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
                // Another writer's write is bounded by agh as this tool's is ([`WRITE`]): an
                // intent open past that has no write running.
                let until = Instant::now() + WRITE;
                remote.wait_for_intent(intent, &|| {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        *self.unsettled.borrow_mut() = Some(format!(
                            "agh intent {} stayed open for {}s, longer than any agh write runs: \
                             it is orphaned, and it is retracted once its write is known not to \
                             have landed (`agh intent list --open`)",
                            intent.as_str(),
                            WRITE.as_secs()
                        ));
                        return Err(format!(
                            "another writer's atomic intent stayed open for {}s",
                            WRITE.as_secs()
                        ));
                    }
                    (self.pause)(left.min(Duration::from_millis(100)));
                    Ok(())
                })
            },
            // The refused attempt sent nothing. Where main is unchanged the same expected-ref
            // push runs again; this is not replay of an unknown write.
            &|| self.observe(landing, onto),
            &|why| self.settle(landing, onto, why),
            &|why| self.again(&self.unsent, "the push", why),
        )
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
enum Composition {
    /// It does: the clean merge's tree and the rule's reason.
    Composes { tree: String, why: String },
    /// It does once the source readers ([`RECHECKS`]) pass on the merge: the clean merge's tree,
    /// and the shared paths, each no-reach Markdown ([`Git::markdown_shared`]).
    Rechecks { tree: String, shared: Vec<String> },
    /// It does not, for this reason: a merge and a new judgment come instead.
    Doomed(String),
}

/// Test main's tip `tip` against `checked` (HEAD, merging `merged`): seconds of Git plumbing,
/// the same whether the judgment still runs or has passed.
fn composition(
    git: &Git<'_>,
    merged: &str,
    tip: &str,
    checked: &str,
) -> Result<Composition, String> {
    let compose = if git.is_ancestor(merged, tip)? {
        Some(git.merge_tree(tip, checked)?)
    } else {
        None
    };
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
            let markdown = match git.markdown_shared(merged, tip, checked, tree)? {
                Ok(shared) => {
                    return Ok(Composition::Rechecks {
                        tree: tree.clone(),
                        shared,
                    });
                }
                Err(why) => why,
            };
            // Why the Markdown rule did not hold, where it was the rule in question.
            let markdown = match &blocked {
                Independence::Blocked { paths, .. }
                    if paths.iter().all(|path| path.ends_with(b".md")) =>
                {
                    format!(" ({markdown})")
                }
                Independence::Blocked { .. } | Independence::Composes(_) => String::new(),
            };
            format!("{blocked}{markdown}")
        }
        Some(Compose::Conflict) => "the merge is not clean".to_owned(),
    };
    Ok(Composition::Doomed(why))
}

/// Judge `at` (HEAD) over the cone since merged main `base`, inheriting its lane's reds
/// once the run ends, outside `branch`'s crates. While it runs, each poll the run asks for
/// reads main's tip ([`Git::fetch_unless`], the landing's own read, held to [`NETWORK`] and ended
/// once the poll is moot) and tests a new one ([`composition`]); the first that does not compose
/// interrupts the run, and nothing it wrote counts: it is stated as interrupted
/// ([`Stated::interrupted`]). A poll that cannot read or test main changes nothing, and says so
/// once per run of one reason, unless it became moot meanwhile. It runs as the checkout's own
/// verifier ([`built`]), which must be this image: else [`Judging::Elsewhere`], before anything
/// ran. The first judgment that runs reads the branch's crates (`held.branch`) over the cone
/// since `held.cone` as that image.
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
        branch,
        cone,
        stated,
        ..
    } = held;
    let branch = &*branch.get_or_insert_with(|| own_crates(git, seams, cone));
    (seams.say)(&format!(
        "verify land: judging {tree} over the cone since {base}"
    ));
    let mut watch = aterm_land::Watch::new(base);
    let ran = {
        let Seams { judge, say, .. } = &mut *seams;
        let mut poll = |moot: Moot<'_>| {
            watch.poll(
                moot,
                &mut |moot| git.fetch_unless(NETWORK, moot),
                &mut |tip| composition(git, base, tip, at).map(engine::composition),
                &mut |why| say(&format!("verify land: {REMOTE}/{MAIN} was not read or tested during the judgment ({why}); it goes on")),
            )
        };
        judge(git.root, &image.path, base, &mut poll)
    };
    let doomed = watch.into_doomed();
    // A run that ended by itself is judged as it ended, whatever a poll answered as it did.
    let interrupted = match (ran, doomed) {
        (Ran::Ended(judged), _) => Err(judged),
        (Ran::Interrupted { secs }, Some((tip, why))) => {
            (seams.say)(&format!(
                "verify land: INTERRUPTED the judgment of {tree}: {REMOTE}/{MAIN} moved to {tip}, \
                 which does not compose: {why}"
            ));
            Ok((tip, secs))
        }
        (Ran::Interrupted { .. }, None) => {
            return Err(format!(
                "the judgment of {tree} was interrupted, though no poll asked for it"
            ));
        }
    };
    if let Some(why) = git.moved_from(at)? {
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
    // Read once the run ends: the lane judges main on its own schedule, so a verdict on `base`
    // that came while this ran counts, and none can depend on the branch.
    let lane = (seams.lane)(git.root, base);
    (seams.say)(&match &lane {
        Lane::Red(items) => format!(
            "verify land: main's lane judged {base} red ({} items)",
            items.len()
        ),
        Lane::Pass => format!("verify land: main's lane judged {base} green"),
        Lane::Unjudged(why) => format!("verify land: main's lane has not judged {base}: {why}"),
    });
    let decision = decide(&judged, &lane, branch);
    for id in &decision.inherited {
        (seams.say)(&format!(
            "verify land: INHERITED {id} (main's lane named it red at {base}, outside the branch's crates)"
        ));
    }
    if !decision.counted.is_empty() {
        (seams.say)(&format!(
            "verify land: FAIL — {} failed; nothing was pushed",
            decision.counted.join(", ")
        ));
    }
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
    for id in decision.inherited {
        if !stated.inherited.contains(&id) {
            stated.inherited.push(id);
        }
    }
    Ok(Judging::Passed)
}

/// Recheck `at` (HEAD, the merge of main tip `tip` a Markdown compose made) with the source
/// readers over the cone since `tip` ([`Seams::recheck`]), as the merge's own verifier
/// ([`built`]), which must be this image: else [`Rechecking::Elsewhere`], before anything ran.
/// Nothing is inherited: a recheck reads no lane, so every red counts. It passes only when
/// each of [`RECHECKS`] ran and said `ok`, and the checkout is still at `at`, clean: its
/// recheck is then added to `held.stated`.
fn recheck(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    at: &str,
    tip: &str,
    held: &mut Held<'_>,
) -> Result<Rechecking, String> {
    let tree = git.tree(at)?;
    let image = match built(git, seams, (at, tip, &tree), held)? {
        Built::Here(image) => image,
        Built::Elsewhere(image) => return Ok(Rechecking::Elsewhere(image)),
        Built::Exit(code) => return Ok(Rechecking::Exit(code)),
    };
    let stated = &mut held.stated;
    (seams.say)(&format!(
        "verify land: rechecking {tree} with {} over the cone since {tip}",
        RECHECKS.join(", ")
    ));
    let judged = (seams.recheck)(git.root, &image.path, tip);
    if let Some(why) = git.moved_from(at)? {
        (seams.say)(&format!(
            "verify land: COULD NOT RUN — during the recheck, {why}; nothing was pushed"
        ));
        return Ok(Rechecking::Exit(exit::COULD_NOT_RUN));
    }
    let none = || "a recheck inherits nothing".to_owned();
    let mut decision = decide(
        &judged,
        &Lane::Unjudged(none()),
        &Branch::Everything(none()),
    );
    let ran: BTreeSet<&str> = judged.checks.iter().map(|row| row.check.as_str()).collect();
    if ran != RECHECKS.into_iter().collect() {
        decision.not_run.push(format!(
            "the recheck ran {}, not {}",
            ran.into_iter().collect::<Vec<_>>().join(", "),
            RECHECKS.join(", ")
        ));
    }
    for row in &judged.checks {
        if row.word == "skip" {
            decision.not_run.push(format!("{} (skipped)", row.check));
        }
    }
    if !decision.counted.is_empty() {
        (seams.say)(&format!(
            "verify land: FAIL — the recheck of {tree}: {} failed; nothing was pushed",
            decision.counted.join(", ")
        ));
        return Ok(Rechecking::Exit(exit::FAILED));
    }
    if !decision.not_run.is_empty() {
        (seams.say)(&format!(
            "verify land: COULD NOT RUN — the recheck of {tree}: {} did not run; nothing was \
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

fn conflicted(seams: &mut Seams<'_>, main: &str, paths: &[Vec<u8>]) -> i32 {
    (seams.say)(&format!(
        "verify land: FAIL — merging {main} conflicts in {}; nothing was pushed. Fix: resolve \
         them, commit the merge and run it again",
        paths
            .iter()
            .map(|path| display_git_path(path))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    exit::FAILED
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
    if !stated.inherited.is_empty() {
        line.push_str(&format!(", {} inherited", stated.inherited.len()));
    }
    line
}

/// How a merge of main into the checkout went.
enum Merge {
    /// Merged and committed.
    Clean,
    /// Stopped on conflicts, left for the person: the conflicted paths.
    Conflict(Vec<Vec<u8>>),
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
/// a conflict, after which an ordinary worktree merge stops for the person.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Compose {
    /// The clean merge's tree.
    Tree(String),
    /// Git's merge conflicts.
    Conflict,
}

impl Compose {
    /// The clean merge's tree, where there is one.
    fn tree(&self) -> Option<&str> {
        match self {
            Compose::Tree(tree) => Some(tree),
            Compose::Conflict => None,
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
    /// THE MARKDOWN RULE (`docs/DESIGN-landing-economy.md` §2.1): whether checked commit
    /// `checked` (judged over the cone since `base`) composes onto main's tip `main` once the
    /// source readers pass on their clean merge `tree`, where [`Self::independent_changes`]
    /// found shared paths. `Ok(paths)` names every path both sides changed: each is
    /// - in the no-reach class over the merge's own layout ([`aterm_census::no_reach::class`]):
    ///   each shared path after the convergence discard (a shared `Cargo.lock` is not, so a
    ///   lock beside Markdown is judged), and each path the merge holds unlike both sides;
    /// - a regular non-executable file (mode 100644) in `checked`, `main` and the merge, so a
    ///   rename on either side, a symlink, a gitlink or a deletion is judged;
    /// - read by no file of the merge by [`aterm_census::no_reach::may_read`] (a Rust file, or a
    ///   crate's Markdown, outside `crates/aterm-census` naming its file name, a docs directory
    ///   above it or, for a spec table, `SPECS.md` from its own crate, in code or a doctest):
    ///   the census guard's backstop for a read it cannot resolve;
    ///
    /// and the merge holds this land's own no-reach rule ([`Self::holds_this_rule`]), so the
    /// class this binary read is the one census's guard holds the merge to. `Err(why)` says
    /// which part does not hold. Unavailable entry evidence keeps every shared
    /// path, as [`Self::independent_changes`] does.
    fn markdown_shared(
        &self,
        base: &str,
        main: &str,
        checked: &str,
        tree: &str,
    ) -> Result<Result<Vec<String>, String>, String> {
        let theirs = self.files(base, main)?;
        let ours = self.files(base, checked)?;
        let mut shared: BTreeSet<Vec<u8>> = theirs.intersection(&ours).cloned().collect();
        if let (Ok(main_changes), Ok(merge_changes)) = (
            self.entry_changes(checked, main),
            self.entry_changes(checked, tree),
        ) {
            shared.retain(|path| main_changes.contains(path) || merge_changes.contains(path));
        }
        let merged: BTreeSet<Vec<u8>> = self
            .files(main, tree)?
            .intersection(&self.files(checked, tree)?)
            .cloned()
            .collect();
        shared.extend(merged);
        if shared.is_empty() {
            return Ok(Err("no path is shared".into()));
        }
        let mut paths = Vec::new();
        for path in &shared {
            match std::str::from_utf8(path) {
                Ok(path) => paths.push(path.to_owned()),
                Err(_) => {
                    return Ok(Err(format!(
                        "{} is no Markdown the rule names",
                        display_git_path(path)
                    )));
                }
            }
        }
        let listing = self.paths(&["ls-tree", "-r", "-z", "--name-only", "--full-tree", tree])?;
        let manifest = self
            .local(&["cat-file", "blob", &format!("{tree}:Cargo.toml")])
            .ok();
        let layout = aterm_census::no_reach::Layout::new(
            manifest.as_deref(),
            listing
                .iter()
                .filter_map(|path| std::str::from_utf8(path).ok()),
        );
        if let Some(path) = paths
            .iter()
            .find(|path| !aterm_census::no_reach::class(path, &layout))
        {
            return Ok(Err(format!(
                "{path} is not Markdown no build, include or test reads"
            )));
        }
        for commit in [checked, main, tree] {
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
            if let Some(path) = paths.iter().find(|path| !regular.contains(*path)) {
                return Ok(Err(format!(
                    "{path} is not one regular file in {commit}, the branch's and main's tips \
                     and their merge"
                )));
            }
        }
        if !self.holds_this_rule(tree)? {
            return Ok(Err(format!(
                "the merge's {RULE_PATH} is not the rule this land chose the shared paths by"
            )));
        }
        for path in &paths {
            if let Some(file) = self.names_in_code(tree, path)? {
                return Ok(Err(format!(
                    "{path} is named in {file}'s code, so a build or test may read it"
                )));
            }
        }
        Ok(Ok(paths))
    }

    /// The first file of `tree` that may read no-reach `path` ([`aterm_census::no_reach::may_read`]:
    /// a Rust file, or a crate's Markdown, naming one of the path's
    /// [`aterm_census::no_reach::needles`] in code), or `None`.
    fn names_in_code(&self, tree: &str, path: &str) -> Result<Option<String>, String> {
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
            Some(1) => return Ok(None),
            _ => {
                return Err(format!(
                    "git grep {path} {tree}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        let prefix = format!("{tree}:");
        for found in out.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let found = String::from_utf8_lossy(found);
            let file = found.strip_prefix(&prefix).unwrap_or(&found);
            if !aterm_census::no_reach::searched(file) {
                continue;
            }
            let text = self.complete(&["cat-file", "blob", &format!("{tree}:{file}")], LOCAL)?;
            if !text.status.success() {
                return Err(format!(
                    "git cat-file {tree}:{file}: {}",
                    String::from_utf8_lossy(&text.stderr).trim()
                ));
            }
            if aterm_census::no_reach::may_read(file, &String::from_utf8_lossy(&text.stdout), path)
            {
                return Ok(Some(file.to_owned()));
            }
        }
        Ok(None)
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

    fn status(&self, args: &[&str], within: Duration) -> Result<GitStatus, String> {
        let mut cmd = (self.make)(self.root);
        cmd.args(args);
        let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + within)
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        Ok(GitStatus {
            status: out.status,
            stderr: out.stderr,
        })
    }

    fn complete(&self, args: &[&str], within: Duration) -> Result<std::process::Output, String> {
        let mut cmd = (self.make)(self.root);
        cmd.args(args);
        aterm_uds::deadline::output_until_complete_stdout_with_limit(
            &mut cmd,
            Instant::now() + within,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|error| format!("git {}: {error}", args.join(" ")))
    }

    fn ok(&self, args: &[&str], within: Duration) -> Result<String, String> {
        let out = self.complete(args, within)?;
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
            crate::remote::run(self.root, self.make, tool, args, until)
        };
        crate::remote::Remote::open(self.root, REMOTE, &run)?.admit_push_source()
    }

    /// Main's tip, fetched into its remote-tracking ref (which the harness's probe reads): its
    /// remote steps held to [`NETWORK`] from now, the ref update to [`LOCAL`].
    fn fetch(&self) -> Result<String, String> {
        let until = Instant::now() + NETWORK;
        self.fetch_through(&|tool, args| {
            crate::remote::run(self.root, self.make, tool, args, until)
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

    /// Main's tip read through `run`, then written to its remote-tracking ref.
    fn fetch_through(&self, run: &crate::remote::Runner<'_>) -> Result<String, String> {
        let remote = crate::remote::Remote::open(self.root, REMOTE, run)?;
        let tip = remote.refresh_main()?;
        self.local(&["update-ref", &format!("refs/remotes/{REMOTE}/{MAIN}"), &tip])?;
        Ok(tip)
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
    /// conflict, which requires an ordinary worktree merge.
    fn merge_tree(&self, main: &str, checked: &str) -> Result<Compose, String> {
        let out = self.complete(
            &["merge-tree", "--write-tree", "--no-messages", main, checked],
            LOCAL,
        )?;
        let first = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        match out.status.code() {
            Some(0) if aterm_types::land_trailer::object_id(&first) => Ok(Compose::Tree(first)),
            Some(1) => Ok(Compose::Conflict),
            _ => Err(format!(
                "git merge-tree {main} {checked}: {}",
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
    /// staged merge the next run cannot start from.
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
        if !conflicted.is_empty() {
            return Ok(Merge::Conflict(conflicted.into_iter().collect()));
        }
        // A commit's stdout is diagnostic, not a data protocol. Its owning exit decides
        // success; complete capture remains necessary for the repository reads above.
        const COMMIT: &[&str] = &["commit", "--quiet", "--no-verify", "--no-edit"];
        let committed = self
            .status(COMMIT, LOCAL)
            .map_err(|why| self.abort_own_merge(&tip, main, why))?;
        if !committed.status.success() {
            return Err(self.abort_own_merge(
                &tip,
                main,
                format!(
                    "git {}: {}",
                    COMMIT.join(" "),
                    String::from_utf8_lossy(&committed.stderr).trim()
                ),
            ));
        }
        self.update_submodules()?;
        Ok(Merge::Clean)
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
            if stage != "0" || !aterm_types::land_trailer::object_id(pin) {
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
        let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + LOCAL)
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

    /// The judgment kept at [`KEPT`], where it holds for the checkout at `head`: `None` where
    /// none is kept, else the judgment or why this run does not resume it. It holds only where
    /// its record is whole (the judged commit and the main commit it merged; a receipt whose
    /// last judgment or recheck names the record's own tree and that main commit, composed onto
    /// nothing), `head` is the judged commit and holds the judged tree, and `head` holds that
    /// main commit: a judgment passes only on the tree it ran on, over the cone it was computed
    /// from.
    fn kept(&self, head: &str) -> Result<Option<Result<Kept, String>>, String> {
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
        if judged_at != head {
            return Ok(Some(Err(format!(
                "it judged {judged_at}, and HEAD is {head}"
            ))));
        }
        let tree = self.tree(head)?;
        if tree != judged {
            return Ok(Some(Err(format!(
                "it judged {judged}, and HEAD holds {tree}"
            ))));
        }
        if !self.is_ancestor(merged, head)? {
            return Ok(Some(Err(format!(
                "HEAD does not hold {merged}, the main commit it merged"
            ))));
        }
        Ok(Some(Ok(Kept {
            tree,
            merged: merged.clone(),
            stated,
        })))
    }

    /// Forget the kept judgment ([`KEPT`]): its landing is on main. One that does not delete
    /// holds for no checkout whose head is not already on main.
    fn forget(&self) {
        let _ = self.local(&["update-ref", "-d", KEPT]);
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
        "usage: targo --unverified verify land [--admit wait|now] [--timeout <seconds|off>] [--test-cores <n>]\n\n\
         Land this checkout's branch on {REMOTE}/{MAIN}, judged: merge {REMOTE}/{MAIN} (M1), run\n\
         the dev set over the cone since M1, push, and where {MAIN} moved meanwhile compose onto\n\
         it (a clean merge and disjoint or exactly convergent entries, except a sole remaining\n\
         shared Cargo.lock whose isolated local product version alone moved) or merge it and\n\
         judge the branch's delta from that main tip, at most {REJUDGES} times. A {MAIN} that\n\
         moves again is composed onto again, at most {COMPOSES} times in a row. Where the\n\
         shared paths are only Markdown no build, include or test reads (root *.md,\n\
         docs/**/*.md, a workspace member's SPECS.md; census's no-reach-read guard\n\
         holds that set), it merges that tip and reruns only fmt, census and export-scan on the\n\
         merge with the merged tree's own verifier, over the cone since the tip, and states an\n\
         Aterm-Rechecked line; any red there counts. A red main's lane already named at the\n\
         main commit merged (M1, or the moved main a re-judge merged), in a crate outside the\n\
         branch's own, is INHERITED: named, not counted. The landing merge carries\n\
         Aterm-Checked, Aterm-Interrupted, Aterm-Rechecked, Aterm-Composed-Onto and\n\
         Aterm-Inherited trailers.\n\n\
         While a judgment runs, {REMOTE}/{MAIN} is read at each stage boundary the run reports\n\
         and at least every {poll} minutes. The first tip that does not compose by the same\n\
         rule INTERRUPTS the run: its process group is ended, and what that started outside\n\
         it is awaited as it ends by its own parent-death watch. The interrupted run is no\n\
         verdict, and states an Aterm-Interrupted line, never an Aterm-Checked one. While\n\
         re-judges remain, that tip is merged and judged at once, as one of the {REJUDGES}. A tip\n\
         that composes by either rule changes nothing, and a run that already reported a red (FAIL or NOT RUN:\n\
         a check, a stage or a test binary), even one later INHERITED, is not interrupted: its\n\
         end decides.\n\n\
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
         receipt, judging again only where the moved {MAIN} does not compose.\n\n\
         GitHub reads and writes use agh (expected refs, durable write intents; one write, with\n\
         no rehearsal that would repeat agh's re-verification of the pushed history while\n\
         {MAIN} moves); local remotes use git's file protocol only. Required submodules must\n\
         already be initialised:\n\
         landing never clones one. A commit a merged main pins that a submodule lacks is imported,\n\
         exactly, through that submodule's own {REMOTE}, the same way, and is all it fetches there.\n\n\
         Each judgment and recheck runs as the verifier built from the tree it judges: where that\n\
         build is not the running landing, the landing hands itself to it in place and goes on\n\
         ({resume} <file> is that handoff's own word, never a person's).\n\n\
         Exit: 0 landed (or nothing to land), 1 a red or a conflict (nothing pushed), 2 usage,\n\
         3 could not run, {STOPPED} stopped while {MAIN} kept moving (merged and judged, or its\n\
         judgment interrupted; not pushed).\n",
        resume = handoff::RESUME,
        poll = POLL.as_secs() / 60,
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

/// `verify land [--admit wait|now] [--timeout seconds|off] [--test-cores n] [--help]`, from `cwd`'s checkout.
/// `verify land --resume <file>` is the landing's own handoff to another build of itself.
pub fn run(args: &[String], cwd: &Path) -> i32 {
    match args {
        [help] if help == "--help" || help == "-h" => {
            print!("{}", usage());
            return exit::PASS;
        }
        [flag, file] if flag == handoff::RESUME => {
            return resume_cli(Path::new(file), cwd, &mut |line| eprintln!("{line}"));
        }
        _ => {}
    }
    let options = match options(args) {
        Some(options) => options,
        None => {
            eprint!(
                "verify land: takes only --admit wait|now, --timeout seconds|off and --test-cores n\n{}",
                usage()
            );
            return exit::USAGE;
        }
    };
    let Some(image) = crate::report::running_driver() else {
        eprintln!("verify land: COULD NOT RUN — this landing's own image could not be read");
        return exit::COULD_NOT_RUN;
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
    with_live_seams(&options, |seams| {
        land(&root, &Own { words: args, image }, seams)
    })
}

/// `verify land --resume <file>`: take the handoff at `file` (removing it), then resume the
/// landing it holds in `cwd`'s checkout (the checkout's root, where the landing handed itself
/// over) as this image, under the person's `land` words parsed again by this build. Only the
/// checkout's own handoff file is read or removed ([`own_handoff`]). A handoff this build does
/// not read, or words it does not take, could not run: the person typed nothing to this image.
/// Each line such a refusal says goes to `refuse` (stderr, from [`run`]); once resumed, the
/// landing says its lines as [`land`] does.
fn resume_cli(file: &Path, cwd: &Path, refuse: &mut dyn FnMut(&str)) -> i32 {
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
    let handoff = match Handoff::take(file) {
        Ok(handoff) => handoff,
        Err(why) => {
            refuse(&format!(
                "verify land: COULD NOT RUN — the landing's handoff {}: {why}; {RERUN}",
                file.display()
            ));
            return exit::COULD_NOT_RUN;
        }
    };
    let Some(image) = crate::report::running_driver() else {
        refuse(&format!(
            "verify land: COULD NOT RUN — this landing's own image could not be read; {RERUN}"
        ));
        return exit::COULD_NOT_RUN;
    };
    let words = handoff.words.clone();
    let Some(options) = options(&words) else {
        refuse(&format!(
            "verify land: COULD NOT RUN — this tree's verify land does not take the landing's \
             words `{}`; {RERUN}",
            words.join(" ")
        ));
        return exit::COULD_NOT_RUN;
    };
    with_live_seams(&options, |seams| {
        resume(
            &root,
            &Own {
                words: &words,
                image,
            },
            handoff,
            seams,
        )
    })
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

/// A git command in the checkout at `root`, as every live landing step runs one.
fn live_git(root: &Path) -> Command {
    let mut cmd = crate::judged::command(root, &[]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for key in PATHSPEC_MODES {
        cmd.env_remove(key);
    }
    cmd
}

/// Run `f` with the live seams under the person's resource controls `options`.
fn with_live_seams<R>(options: &LandResources, f: impl FnOnce(&mut Seams<'_>) -> R) -> R {
    let mut say = |line: &str| println!("{line}");
    f(&mut Seams {
        git: &live_git,
        verifier: &mut live_verifier,
        judge: &mut |root, verifier, base, poll| live_judge(root, verifier, base, options, poll),
        branch: &mut live_branch,
        lane: &mut live_lane,
        recheck: &mut |root, verifier, base| live_recheck(root, verifier, base, options),
        hand_over: &mut handoff::hand_over,
        say: &mut say,
        remote: &crate::remote::run,
        pause: &std::thread::sleep,
    })
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
    let path = crate::native_verifier::build(root, &tools.targo, &plan)?;
    let digest = crate::test_index::digest(&path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
    Ok(Image { path, digest })
}

/// The dev set over the cone since `base`, run as `verifier`, the checkout's freshly built
/// verifier (so the merged tree's tools judge it) and the landing's own image, its rows read
/// from the check-finished events it keeps and its record's end from its run-finished one.
/// While it runs, `poll` is asked as [`supervise_judgment`] asks it; a run it interrupts is no
/// verdict.
fn live_judge(
    root: &Path,
    verifier: &Path,
    base: &str,
    resources: &LandResources,
    poll: Poll<'_>,
) -> Ran {
    nested(root, verifier, base, &[], resources, poll)
}

/// The source readers ([`RECHECKS`]) over the cone since `base`, run as [`live_judge`] runs the
/// dev set: as `verifier`, the checkout's own verifier built from the merge it rechecks and the
/// landing's own image, so the merged tree's rules judge it and nothing an earlier build left
/// is reused. Nothing interrupts it.
fn live_recheck(root: &Path, verifier: &Path, base: &str, resources: &LandResources) -> Judged {
    match nested(root, verifier, base, &RECHECKS, resources, &mut |_| false) {
        Ran::Ended(judged) => judged,
        Ran::Interrupted { secs } => Judged {
            code: exit::COULD_NOT_RUN,
            secs,
            ..Judged::default()
        },
    }
}

/// `checks` (the dev set where empty) over the cone since `base`, as `verifier`, the
/// checkout's freshly built verifier.
fn nested(
    root: &Path,
    verifier: &Path,
    base: &str,
    checks: &[&str],
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
    let command = judgment_command(verifier, root, base, checks, &events, resources, &env);
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
        // No verdict: nothing it wrote is read.
        Ok(Ending::Interrupted) => {
            return Ran::Interrupted {
                secs: began.elapsed().as_secs(),
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
    fn interrupt(&mut self);
}

/// How a supervised judgment ended ([`supervise_judgment`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The run exited with this code.
    Exited(i32),
    /// The poll answered `true`, and the run was interrupted.
    Interrupted,
}

/// Wait for `run`, asking `poll` at each new stage boundary (read every [`BOUNDARY_TICK`]) and
/// at least every [`POLL`] by `now` whether to interrupt it, and telling it whether the run's own
/// answer came meanwhile ([`answered`]). Once the run reported a red it is not asked again, and
/// the run's own exit ends the supervision: its end decides, through [`decide`] (an inherited red
/// still lands), as without the poll. A `true` interrupts the run at once, unless it exited or
/// reported a red while the poll read main; nothing else interrupts it.
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
    loop {
        let left = poll_at.saturating_duration_since(now());
        if let Some(code) = run.wait(left.min(BOUNDARY_TICK))? {
            return Ok(Ending::Exited(code));
        }
        let report = run.reported();
        if report.unpassed {
            return own_end(run);
        }
        if report.boundaries == seen && now() < poll_at {
            continue;
        }
        seen = report.boundaries;
        if poll(&mut || answered(run)) {
            if let Some(code) = run.wait(Duration::ZERO)? {
                return Ok(Ending::Exited(code));
            }
            if run.reported().unpassed {
                return own_end(run);
            }
            run.interrupt();
            return Ok(Ending::Interrupted);
        }
        poll_at = now() + POLL;
    }
}

/// Whether `run`'s own answer came: it exited (or cannot be waited on), or it reported a red.
fn answered(run: &mut impl Running) -> bool {
    !matches!(run.wait(Duration::ZERO), Ok(None)) || run.reported().unpassed
}

/// Wait for `run`'s own exit, asking nothing.
fn own_end(run: &mut impl Running) -> std::io::Result<Ending> {
    loop {
        if let Some(code) = run.wait(POLL)? {
            return Ok(Ending::Exited(code));
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
        interrupt_judgment(run.child);
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

    fn interrupt(&mut self) {
        interrupt_judgment(self.child);
    }
}

/// What a run's events file reported so far ([`EventsTail::read`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Report {
    /// Its stage boundaries: each complete `stage-finished` record
    /// ([`crate::event_log::observation`]), once.
    boundaries: usize,
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
/// that is not a pass makes it unpassed.
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
/// gone, and its children once their parents are. This returns once all of that is gone, or once
/// [`INTERRUPT_GRACE`] passed with none of it ending: what the run held (its capacity claims, its
/// build's locks) is released
/// as its holders end, so the re-judge after it finds them free. A process one of those starts
/// after the leader is reaped is not awaited. The interrupted run is no verdict.
pub fn interrupt_judgment(child: &mut aterm_uds::spawnfd::Child) {
    interrupt(child, INTERRUPT_GRACE);
}

/// [`interrupt_judgment`], awaiting what the group started outside it until `outside` passed
/// with none of it ending, in place of [`INTERRUPT_GRACE`]: a test passes its hang detector, so
/// that only a hang fails it.
#[cfg(any(test, debug_assertions))]
pub fn interrupt_judgment_awaiting(child: &mut aterm_uds::spawnfd::Child, outside: Duration) {
    interrupt(child, outside);
}

fn interrupt(child: &mut aterm_uds::spawnfd::Child, grace: Duration) {
    let mut group = None;
    let mut outside = Vec::new();
    if child.owns_unreaped()
        && let Ok(leader) = i32::try_from(child.id())
        && leader > 1
    {
        outside = started_outside(leader.unsigned_abs(), &[]).unwrap_or_default();
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
        if let Some(group) = group
            && let Some(now) = started_outside(group, &outside)
        {
            outside = now;
        }
    }
    aterm_uds::deadline::settle_child(child);
    // Each end renews the grace: the chain of watches ends what is left a step at a time. What
    // is awaited only shrinks, so this ends.
    let mut until = Instant::now() + grace;
    loop {
        let before = outside.len();
        outside.retain(Started::running);
        let now = Instant::now();
        if outside.len() < before {
            until = now + grace;
        }
        if outside.is_empty() || now >= until {
            break;
        }
        std::thread::sleep(OUTSIDE_TICK);
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
}

/// [`beyond`] by a read of this user's process table ([`aterm_uds::runs::LiveKernel`]), or
/// `None` where it does not read.
fn started_outside(group: u32, known: &[Started]) -> Option<Vec<Started>> {
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
/// its pid and birth), and theirs in turn, with `known`'s that still run.
fn beyond(rows: &[aterm_uds::process::Lineage], group: u32, known: &[Started]) -> Vec<Started> {
    let mut walked = BTreeSet::new();
    let mut parents = Vec::new();
    let mut found = Vec::new();
    for row in rows {
        let member = row.pgid == group;
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

/// Run the checkout's freshly built verifier directly, owned by the calling land process, on
/// `checks` (the dev set where empty: a judgment; [`RECHECKS`]: a recheck). Its test check
/// brackets each test binary's executable and declared runtime inputs
/// ([`crate::cli::INPUT_DRIFT`]): a successful binary whose inputs changed while it ran makes it
/// NOT RUN, and nothing is carried between judgments.
/// The explicit trampoline uses the caller's current executable, whose `main` must serve
/// [`aterm_uds::tether::entry`]. It does not depend on the image recorded before a rebuild.
/// Linux runs its loaded image through `/proc/self/exe`, which a replaced pathname cannot change.
/// Its words are composed and parsed by one build: a landing whose image is not the checkout's
/// build hands itself over to that build first ([`handoff`]), so these flags are the landing's
/// own, internal, and may change with any main.
/// # Errors
/// This process's executable path could not be read.
pub fn judgment_command(
    verifier: &Path,
    root: &Path,
    base: &str,
    checks: &[&str],
    events: &Path,
    resources: &LandResources,
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
    cmd.args(checks)
        .args(["--base", base, "--events"])
        .arg(events)
        .args([aterm_capacity::Admit::FLAG, resources.admit.word()]);
    if checks.is_empty() || checks.contains(&"test") {
        cmd.arg(crate::cli::INPUT_DRIFT);
    }
    if let Some(timeout) = resources.timeout.as_deref() {
        cmd.args(["--timeout", timeout]);
    }
    if let Some(cores) = resources.test_cores {
        cmd.args(["--test-cores", &cores.to_string()]);
    }
    Ok(cmd)
}

/// The seeds of the cone since `base`, as [`crate::changed::resolve`] reads them for the run.
fn live_branch(root: &Path, base: &str) -> Branch {
    let env = crate::EnvSnapshot::capture();
    let tools = match crate::preflight(&env, root) {
        Ok(tools) => tools,
        Err(why) => return Branch::Everything(format!("the toolchain did not resolve: {why}")),
    };
    let path_env = tools.path_with_stage2_first(&env.path);
    match crate::changed::resolve(root, &tools, &path_env, base) {
        crate::changed::Selection::Widened(why) => Branch::Everything(why),
        crate::changed::Selection::Narrowed { seeds, .. } => Branch::Crates(
            seeds
                .iter()
                .filter_map(|seed| crate::scope::spec_name(seed))
                .map(str::to_owned)
                .collect(),
        ),
    }
}

/// Main's lane verdict on `commit` from B8's journal (`refs/aterm/judged`), by its tree.
fn live_lane(root: &Path, commit: &str) -> Lane {
    let until = crate::judged::deadline();
    let tree = match crate::judged::git(root, &["rev-parse", &format!("{commit}^{{tree}}")]) {
        Ok(tree) => tree,
        Err(e) => return Lane::Unjudged(format!("its tree was not read: {e}")),
    };
    let head = match crate::judged::head(root, REMOTE, until) {
        Ok(head) => head,
        Err(e) => return Lane::Unjudged(format!("the lane's journal was not read: {e}")),
    };
    match crate::judged::record(root, head.as_deref(), &tree, until) {
        Ok(Some(record)) if record.state == "red" => Lane::Red(
            record
                .rows
                .iter()
                .filter(|row| row.outcome == "red")
                .flat_map(|row| row.red_items.iter().cloned())
                .collect(),
        ),
        Ok(Some(record)) if record.state == "pass" => Lane::Pass,
        Ok(Some(record)) => {
            Lane::Unjudged(format!("its verdict is {} ({})", record.state, record.host))
        }
        Ok(None) => Lane::Unjudged("the journal has no verdict on its tree".into()),
        Err(e) => Lane::Unjudged(format!("its verdict was not read: {e}")),
    }
}

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;
