// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified verify land` (design B10, `docs/DESIGN-fast-build-release-2026-11-05.md`
//! §4.6): land this checkout's branch on main, judged.
//!
//! 1. Fetch main and merge it (M1), stopping on a conflict. A `CHANGELOG.md` entry the branch
//!    adds that the merge leaves under a released version (a release cut on main after it was
//!    written) goes back under `## [Unreleased]` ([`changelog`]), in the tool's own merge, else
//!    in a commit of its own; so does one each re-judge's merge (step 4) leaves there.
//! 2. Run the dev set over the cone since M1 (`verify --base M1`, B7's export scan included),
//!    on the committed checkout: one that changes during the run pushes nothing.
//! 3. A red main's lane verdict on the merged main commit names (B8's journal,
//!    [`crate::judged`]; M1, or on a re-judge the moved main it merged; read once the run ends,
//!    so a verdict the lane gave meanwhile counts), in a crate outside the branch's own
//!    ([`Branch`]), is INHERITED: named and not counted. Any other red, and a run
//!    whose record is not whole, exits non-zero, and nothing is pushed.
//! 4. Push (a push git reports failed whose landing main holds landed). Where main moved to M2
//!    meanwhile, compose the pass onto M2 when AGENTS.md's rule holds: a clean merge and
//!    disjoint changed files, except a sole shared `Cargo.lock` whose isolated local product
//!    version alone moved ([`version::independent_lock`]). The four immutable trees retain
//!    every other lock byte and the source-proven binary-leaf identity boundary. That
//!    is seconds of plumbing, so a main that moves again is composed onto again at once
//!    ([`COMPOSES`] in a row at most). Where the rule fails, merge M2 and judge the branch's
//!    delta from M2 against its new dependencies (`verify --base M2`); after [`REJUDGES`]
//!    re-judges it stops with the branch merged and judged, and says so. Semantic breaks
//!    across disjoint files are B8's.
//! 5. The landing merge states what ran ([`aterm_types::land_trailer`]). A branch whose head
//!    already merges the main it lands on is that merge, restated with the trailers.
//!
//! A tool, not a gate: nothing refuses a push made without it, and the harness records a landing
//! without its trailers rather than refusing it.

use crate::exit;
use aterm_types::land_trailer::{Checked, Row, Stated};
use aterm_uds::spawnfd::Command;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

#[path = "land_version.rs"]
mod version;

/// The remote main is fetched from and pushed to.
pub const REMOTE: &str = "origin";
/// The branch the landing moves.
pub const MAIN: &str = "main";
/// How many times a moved main is merged and judged again before the tool stops.
pub const REJUDGES: usize = 3;
/// How many composition and pre-send obstruction races are allowed before the tool stops.
/// A different writer or intent never renews this bound.
pub const COMPOSES: usize = 10;
/// The exit of a landing the tool stopped while main kept moving: the branch is merged and
/// judged, not pushed (`EX_TEMPFAIL`, as a yielding `verify watch --sample`).
pub const STOPPED: i32 = 75;
/// The hang detector on a local git read or write.
const LOCAL: Duration = Duration::from_secs(60);
/// The hang detector on a git step that reaches the network or checks out a merge.
const NETWORK: Duration = Duration::from_secs(600);

/// What the freshly fetched main proves after another writer's pre-send obstruction closes.
enum PushObservation {
    Landed,
    Unchanged,
    Moved(String),
}

/// The ordinary push and its existing refusal policy are supplied unchanged. Only an exact
/// pre-send obstruction permits another attempt, sharing the composition bound with main
/// movement. The live callbacks all share the original push's absolute network deadline.
fn push_after_intents(
    composes: &mut usize,
    attempt: &dyn Fn() -> Result<crate::remote::Pushed, String>,
    wait: &dyn Fn(&agh::IntentId) -> Result<(), String>,
    observe: &dyn Fn() -> Result<PushObservation, String>,
    refused: &dyn Fn(&str) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    loop {
        match attempt()? {
            crate::remote::Pushed::Done => return Ok(None),
            crate::remote::Pushed::Rejected(why) => return refused(&why),
            crate::remote::Pushed::Blocked { intent, why } => {
                if *composes == COMPOSES {
                    return Err(format!(
                        "{why}; stopped after {COMPOSES} push/composition races"
                    ));
                }
                *composes += 1;
                wait(&intent)?;
                match observe()? {
                    PushObservation::Landed => return Ok(None),
                    PushObservation::Moved(tip) => return Ok(Some(tip)),
                    PushObservation::Unchanged => {}
                }
            }
        }
    }
}

pub use crate::event_log::{CheckRow, Finished, Judged};

/// The branch's own crates: a red in one of them is the branch's, whatever main's lane says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Branch {
    /// The crates the change itself touched (the cone's seeds), by name.
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

/// What `verify land` calls out to. Production passes [`run`]'s live seams; tests pass scripted
/// ones over scratch repositories.
pub struct Seams<'a> {
    /// A git command in the checkout.
    pub git: &'a dyn Fn(&Path) -> Command,
    /// Run the dev set on the checkout as it stands over the cone since a base commit.
    pub judge: &'a mut dyn FnMut(&Path, &str) -> Judged,
    /// The branch's own crates: the seeds of the cone since the merged main commit.
    pub branch: &'a mut dyn FnMut(&Path, &str) -> Branch,
    /// Main's lane verdict on a main commit, read once each judgment ends.
    pub lane: &'a mut dyn FnMut(&Path, &str) -> Lane,
    /// Say a line to the person.
    pub say: &'a mut dyn FnMut(&str),
}

/// Land the checkout at `root` ([`Seams`]): the exit code ([`exit`], or [`STOPPED`]).
pub fn land(root: &Path, seams: &mut Seams<'_>) -> i32 {
    let git = Git {
        root,
        make: seams.git,
    };
    match drive(&git, seams) {
        Ok(code) => code,
        Err(why) => {
            (seams.say)(&format!("verify land: COULD NOT RUN — {why}"));
            exit::COULD_NOT_RUN
        }
    }
}

fn drive(git: &Git<'_>, seams: &mut Seams<'_>) -> Result<i32, String> {
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
    let label = git
        .local(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|_| "HEAD".into());
    let subject = format!("Merge {REMOTE}/{MAIN} into {label}");
    let mut merged = git.fetch()?;
    let start = git.head()?;
    if git.is_ancestor(&start, &merged)? {
        say(
            seams,
            format!("verify land: nothing to land — HEAD {start} is on {REMOTE}/{MAIN}"),
        );
        return Ok(exit::PASS);
    }
    let merged_here = !git.is_ancestor(&merged, &start)?;
    if merged_here && let Merge::Conflict(paths) = git.merge(&merged, &subject)? {
        return Ok(conflicted(seams, &merged, &paths));
    }
    rehome_changelog(git, seams, &merged, merged_here.then_some(subject.as_str()))?;
    let mut judged_at = git.head()?;
    let branch = (seams.branch)(git.root, &merged);
    say(
        seams,
        match &branch {
            Branch::Crates(crates) => format!(
                "verify land: the branch's crates: {}",
                crates.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
            Branch::Everything(why) => {
                format!("verify land: the branch reaches every crate ({why}): no red is inherited")
            }
        },
    );
    let mut stated = Stated::default();
    if let Some(code) = judge(git, seams, &judged_at, &merged, &branch, &mut stated)? {
        return Ok(code);
    }
    let mut onto = merged.clone();
    let mut composed_tree: Option<String> = None;
    let (mut rejudges, mut composes) = (0, 0);
    let mut push_until = Instant::now() + NETWORK;
    loop {
        stated.composed_onto = (onto != merged).then(|| onto.clone());
        let message = format!("{subject}\n\n{}", stated.trailers());
        let parents = git.parents(&judged_at)?;
        let landing = match &composed_tree {
            Some(tree) => git.commit(tree, &[&judged_at, &onto], &message)?,
            // The judged commit already merges the main it lands on (the tool's own merge, or
            // one made by hand): that merge, restated with its own message and the trailers.
            None if parents.len() == 2 && parents[1] == onto => {
                let own = git.local(&["log", "-1", "--format=%B", &judged_at])?;
                let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
                git.commit(
                    &git.tree(&judged_at)?,
                    &parents,
                    &format!("{own}\n\n{}", stated.trailers()),
                )?
            }
            None => git.commit(&git.tree(&judged_at)?, &[&judged_at, &onto], &message)?,
        };
        let tip = match git.push(&landing, &onto, &mut composes, push_until)? {
            None => {
                let adopted = git.adopt(&landing, &judged_at);
                say(seams, landed_line(&landing, &stated));
                if let Err(why) = adopted {
                    say(
                        seams,
                        format!("verify land: the landing is on {REMOTE}/{MAIN}, but {why}"),
                    );
                }
                return Ok(exit::PASS);
            }
            Some(tip) => tip,
        };
        say(
            seams,
            format!("verify land: {REMOTE}/{MAIN} moved to {tip} before the push"),
        );
        let tree = if git.is_ancestor(&merged, &tip)? {
            git.merge_tree(&tip, &judged_at)?
        } else {
            None
        };
        let overlap = match &tree {
            Some(_) => {
                let theirs = git.files(&merged, &tip)?;
                let ours = git.files(&merged, &judged_at)?;
                theirs.intersection(&ours).cloned().collect::<Vec<_>>()
            }
            None => Vec::new(),
        };
        let composition = tree
            .as_ref()
            .map(|tree| git.independent_changes(&merged, &tip, &judged_at, tree))
            .transpose()?
            .flatten();
        if let (Some(tree), Some(why)) = (&tree, composition) {
            if composes == COMPOSES {
                say(
                    seams,
                    format!(
                        "verify land: STOPPED after {COMPOSES} push/composition races; \
                         the branch is merged with {merged} and judged, and \
                         nothing was pushed. Run it again"
                    ),
                );
                return Ok(STOPPED);
            }
            composes += 1;
            say(seams, format!("verify land: composing onto {tip}: {why}"));
            composed_tree = Some(tree.clone());
            onto = tip;
            continue;
        }
        say(
            seams,
            if tree.is_none() {
                format!("verify land: {tip} does not compose: the merge is not clean")
            } else {
                format!(
                    "verify land: {tip} does not compose: its new commits touch the branch's {}",
                    overlap
                        .iter()
                        .map(|path| display_git_path(path))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        );
        if let Some(why) = git.moved_from(&judged_at)? {
            say(
                seams,
                format!("verify land: COULD NOT RUN — {why}; nothing was pushed"),
            );
            return Ok(exit::COULD_NOT_RUN);
        }
        if rejudges == REJUDGES {
            say(
                seams,
                format!(
                    "verify land: STOPPED after {REJUDGES} re-judges — the branch is merged with \
                     {merged} and judged, and nothing was pushed. Run it again"
                ),
            );
            return Ok(STOPPED);
        }
        match git.merge(&tip, &subject)? {
            Merge::Conflict(paths) => return Ok(conflicted(seams, &tip, &paths)),
            Merge::Clean => {
                rehome_changelog(git, seams, &tip, Some(&subject))?;
                judged_at = git.head()?;
                rejudges += 1;
                composes = 0;
                composed_tree = None;
                merged = tip.clone();
                onto = tip;
                // Main's intervening edits are already in `merged`. Only the branch's delta
                // from that tip can change its outcomes, including in the branch's dependents.
                // The merged checkout still compiles and tests against main's new dependencies.
                if let Some(code) = judge(git, seams, &judged_at, &merged, &branch, &mut stated)? {
                    return Ok(code);
                }
                push_until = Instant::now() + NETWORK;
            }
        }
    }
}

/// Judge `at` (HEAD) over the cone since merged main `base`, inheriting its lane's reds
/// once the run ends, outside `branch`'s crates: `None` when it passed (its judgment added to `stated`),
/// else the exit to stop with.
fn judge(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    at: &str,
    base: &str,
    branch: &Branch,
    stated: &mut Stated,
) -> Result<Option<i32>, String> {
    let tree = git.tree(at)?;
    (seams.say)(&format!(
        "verify land: judging {tree} over the cone since {base}"
    ));
    let judged = (seams.judge)(git.root, base);
    if let Some(why) = git.moved_from(at)? {
        (seams.say)(&format!(
            "verify land: COULD NOT RUN — during the run, {why}; nothing was pushed"
        ));
        return Ok(Some(exit::COULD_NOT_RUN));
    }
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
        return Ok(Some(exit::FAILED));
    }
    if !decision.not_run.is_empty() {
        return Ok(Some(exit::COULD_NOT_RUN));
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
    Ok(None)
}

/// Move the `CHANGELOG.md` entries the checkout adds beside main commit `main` that sit under a
/// released version back under `## [Unreleased]`, or drop one that copies an entry the file
/// holds elsewhere ([`changelog::rehome`]), and commit the change:
/// into HEAD, restated as `merge` (the tool's own merge's subject), else as a commit of its own.
fn rehome_changelog(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    main: &str,
    merge: Option<&str>,
) -> Result<(), String> {
    let file = git.root.join(changelog::PATH);
    let theirs = format!("{main}:{}", changelog::PATH);
    if !file.is_file()
        || git
            .status(&["cat-file", "-e", &theirs], LOCAL)?
            .status
            .code()
            != Some(0)
    {
        return Ok(());
    }
    let diff = git.local(&[
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--unified=0",
        "--inter-hunk-context=0",
        main,
        "HEAD",
        "--",
        changelog::PATH,
    ])?;
    let show = |commit: &str| git.local(&["show", &format!("{commit}:{}", changelog::PATH)]);
    let text =
        std::fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    if text.contains('\r') {
        (seams.say)(&format!(
            "verify land: {} holds a carriage return, so its entries were not checked across \
             the merge of {main}",
            changelog::PATH
        ));
        return Ok(());
    }
    let added = changelog::added_lines(&diff);
    let theirs = show(main)?;
    let wanted = changelog::in_question(&text, &added, &theirs);
    let written = if wanted.is_empty() {
        changelog::Written::new()
    } else {
        written_since(git, main, &wanted)?
    };
    let Some(rehomed) = changelog::rehome(&text, &added, &theirs, &written) else {
        return Ok(());
    };
    let said = changelog::said(&rehomed);
    let message = match merge {
        Some(subject) => format!("{subject}\n\nverify land {said}"),
        None => format!(
            "verify land: the CHANGELOG entries this branch adds sit under [Unreleased]\n\n\
             verify land {said}"
        ),
    };
    let mut commit = vec!["commit", "--quiet", "--no-verify", "-m", message.as_str()];
    if merge.is_some() {
        commit.push("--amend");
    }
    std::fs::write(&file, &rehomed.text).map_err(|e| format!("write {}: {e}", file.display()))?;
    // Put the file back as the merge left it (and the index, once it took the change), so a
    // refused step leaves the checkout committed and clean.
    let put_back = |why: String, staged: bool| {
        let restored = std::fs::write(&file, &text)
            .map_err(|e| format!("write {}: {e}", file.display()))
            .and_then(|()| {
                if staged {
                    git.local(&["add", "--", changelog::PATH]).map(drop)
                } else {
                    Ok(())
                }
            });
        match restored {
            Ok(()) => format!("{why}; {} is as the merge left it", changelog::PATH),
            Err(again) => format!(
                "{why}; putting {} back failed too: {again}",
                changelog::PATH
            ),
        }
    };
    git.local(&["add", "--", changelog::PATH])
        .map_err(|why| put_back(why, false))?;
    git.local(&commit).map_err(|why| put_back(why, true))?;
    (seams.say)(&format!("verify land: {said} (merging {main})"));
    Ok(())
}

/// The `CHANGELOG.md` entries the branch's own commits wrote ([`changelog::Written`]): each
/// commit in `main..HEAD`, oldest first, whose changelog differs from every parent's, against its
/// parents' ([`changelog::wrote`]), each placing only the entries `wanted`
/// ([`changelog::in_question`]). Each changelog is read and scanned once, by its blob, however
/// many commits hold it.
fn written_since(
    git: &Git<'_>,
    main: &str,
    wanted: &BTreeSet<String>,
) -> Result<changelog::Written, String> {
    let mut blobs: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut blob = |commit: &str| -> Option<String> {
        blobs
            .entry(commit.to_owned())
            .or_insert_with(|| {
                git.local(&[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{commit}:{}", changelog::PATH),
                ])
                .ok()
            })
            .clone()
    };
    let mut placed: BTreeMap<String, changelog::Placed> = BTreeMap::new();
    let mut written = changelog::Written::new();
    for line in git
        .local(&[
            "rev-list",
            "--topo-order",
            "--reverse",
            "--parents",
            &format!("{main}..HEAD"),
        ])?
        .lines()
    {
        let mut ids = line.split_whitespace();
        let Some(commit) = ids.next() else {
            continue;
        };
        let Some(own) = blob(commit) else {
            continue;
        };
        let theirs: Vec<String> = ids.filter_map(&mut blob).collect();
        if theirs.contains(&own) {
            continue;
        }
        for id in theirs.iter().chain([&own]) {
            if !placed.contains_key(id) {
                let text = git.local(&["cat-file", "blob", id])?;
                placed.insert(id.clone(), changelog::placed(&text, wanted));
            }
        }
        let before: Vec<&changelog::Placed> = theirs.iter().map(|id| &placed[id]).collect();
        changelog::wrote(&mut written, &placed[&own], &before);
    }
    Ok(written)
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

fn landed_line(landing: &str, stated: &Stated) -> String {
    let last = stated.checked.last().map_or("", |c| c.tree.as_str());
    let mut line = format!(
        "verify land: LANDED {landing} on {REMOTE}/{MAIN}: checked {last} ({} re-judge{})",
        stated.rejudges(),
        if stated.rejudges() == 1 { "" } else { "s" }
    );
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
        let mut command = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        command
            .current_dir(root)
            .env("PATH", path)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALLOW_PROTOCOL", "");
        for key in crate::changed::GIT_REDIRECTS {
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
    if git.merge_tree(checked, main)?.as_deref() != Some(tree) {
        return Ok(false);
    }
    Ok(git
        .independent_changes(base, main, checked, tree)?
        .is_some())
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
    /// The writer and historical reader apply one composition rule, including the
    /// isolated product-version exception. The returned reason is the writer's report.
    fn independent_changes(
        &self,
        base: &str,
        main: &str,
        checked: &str,
        tree: &str,
    ) -> Result<Option<String>, String> {
        let theirs = self.files(base, main)?;
        let ours = self.files(base, checked)?;
        let overlap: Vec<_> = theirs.intersection(&ours).collect();
        if overlap.is_empty() {
            return Ok(Some(
                "the merge is clean and its new commits touch none of the branch's files".into(),
            ));
        }
        if overlap.len() == 1
            && overlap[0].as_slice() == b"Cargo.lock"
            && let Some((old, new)) = version::independent_lock(self, base, main, checked, tree)
        {
            return Ok(Some(format!(
                "the merge is clean and its only shared file is Cargo.lock, where the isolated product version moved from {old} to {new}; every other lock byte is preserved"
            )));
        }
        Ok(None)
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

    /// Main's tip, fetched into its remote-tracking ref (which the harness's probe reads).
    fn fetch(&self) -> Result<String, String> {
        let until = Instant::now() + NETWORK;
        let run = |tool, args: &[std::ffi::OsString]| {
            crate::remote::run(self.root, self.make, tool, args, until)
        };
        let remote = crate::remote::Remote::open(self.root, REMOTE, &run)?;
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

    /// The tree a clean merge of `ours` and `theirs` makes, or `None` where it conflicts.
    fn merge_tree(&self, ours: &str, theirs: &str) -> Result<Option<String>, String> {
        let out = self.complete(
            &["merge-tree", "--write-tree", "--no-messages", ours, theirs],
            LOCAL,
        )?;
        let first = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        match out.status.code() {
            Some(0) if aterm_types::land_trailer::object_id(&first) => Ok(Some(first)),
            Some(1) => Ok(None),
            _ => Err(format!(
                "git merge-tree {ours} {theirs}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// Merge main commit `main` into the checkout and update its submodules.
    fn merge(&self, main: &str, subject: &str) -> Result<Merge, String> {
        let out = self.status(
            &[
                "-c",
                "submodule.recurse=false",
                "merge",
                "--no-ff",
                "--no-edit",
                "--no-verify",
                "-m",
                subject,
                main,
            ],
            NETWORK,
        )?;
        if !out.status.success() {
            let conflicted = self.paths(&["diff", "-z", "--name-only", "--diff-filter=U"])?;
            if conflicted.is_empty() {
                return Err(format!(
                    "git merge {main}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            return Ok(Merge::Conflict(conflicted.into_iter().collect()));
        }
        self.update_submodules()?;
        Ok(Merge::Clean)
    }

    /// Checkout only already imported submodule objects. `--init` could clone through raw git,
    /// even with `--no-fetch`, so an uninitialised submodule is a named prerequisite instead.
    fn update_submodules(&self) -> Result<(), String> {
        let status = self.local(&["submodule", "status", "--recursive"])?;
        if status.lines().any(|line| line.starts_with('-')) {
            return Err("initialise required submodules through agh before verify land".into());
        }
        self.ok(&["submodule", "update", "--no-fetch"], NETWORK)
            .map(drop)
    }

    fn commit(&self, tree: &str, parents: &[&str], message: &str) -> Result<String, String> {
        let mut args = vec!["commit-tree", tree];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        args.extend(["-m", message]);
        self.local(&args)
    }

    /// Push `landing` to main, expecting main at `onto`: `None` once it landed, or main's new
    /// tip where it moved ([`Self::settle`]).
    fn push(
        &self,
        landing: &str,
        onto: &str,
        composes: &mut usize,
        until: Instant,
    ) -> Result<Option<String>, String> {
        let run = |tool, args: &[std::ffi::OsString]| {
            crate::remote::run(self.root, self.make, tool, args, until)
        };
        let remote = crate::remote::Remote::open(self.root, REMOTE, &run)?;
        push_after_intents(
            composes,
            &|| {
                if Instant::now() >= until {
                    return Err("atomic push exceeded its network deadline".into());
                }
                remote.push(&format!("refs/heads/{MAIN}"), Some(onto), landing)
            },
            &|intent| {
                remote.wait_for_intent(intent, &|| {
                    let remaining = until.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(
                            "foreign atomic intent remained open at the push deadline".into()
                        );
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(100)));
                    Ok(())
                })
            },
            &|| {
                let tip = remote.refresh_main()?;
                self.local(&["update-ref", &format!("refs/remotes/{REMOTE}/{MAIN}"), &tip])?;
                if tip == landing || self.is_ancestor(landing, &tip)? {
                    return Ok(PushObservation::Landed);
                }
                if tip != onto {
                    return Ok(PushObservation::Moved(tip));
                }
                // The refused attempt sent nothing and main is unchanged. Re-run both
                // dry-run and expected-ref push; this is not replay of an unknown write.
                Ok(PushObservation::Unchanged)
            },
            &|why| self.settle(landing, onto, why),
        )
    }

    /// Where a push of `landing` onto `onto` git reported failed (`why`) left main, fetched: a
    /// main that holds the landing took it (a connection lost after the server moved the ref
    /// loses only its report), `None`; one still at `onto` refused it, an error and never a
    /// retry; else main moved, its new tip.
    fn settle(&self, landing: &str, onto: &str, why: &str) -> Result<Option<String>, String> {
        let tip = self.fetch()?;
        if tip == landing || self.is_ancestor(landing, &tip)? {
            return Ok(None);
        }
        if tip == onto {
            return Err(why.to_owned());
        }
        Ok(Some(tip))
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
            return self
                .update_submodules()
                .map_err(|why| {
                    format!(
                        "this checkout's submodules were not updated to it: {why}. Fix: \
                         import its required objects through agh and run `git submodule update --no-fetch`"
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
        "usage: targo --unverified verify land [--admit wait|now]\n\n\
         Land this checkout's branch on {REMOTE}/{MAIN}, judged: merge {REMOTE}/{MAIN} (M1), run\n\
         the dev set over the cone since M1, push, and where {MAIN} moved meanwhile compose onto\n\
         it (a clean merge and disjoint files, except a sole shared Cargo.lock whose isolated\n\
         local product version alone moved) or merge it and\n\
         judge the branch's delta from that main tip, at most {REJUDGES} times. A red main's lane already named at the\n\
         main commit merged (M1, or the moved main a re-judge merged), in a crate outside the\n\
         branch's own, is INHERITED: named, not counted. The landing merge carries\n\
         Aterm-Checked, Aterm-Composed-Onto and Aterm-Inherited trailers.\n\n\
         The lock exception validates four immutable trees, preserving every other lock byte,\n\
         dependency, format and mode. Uncertain evidence is judged again. Aterm-Checked still\n\
         names the tree that ran; a composed product identity is not that earlier executable.\n\n\
         --admit changes resource admission only, forwarded to every judgment. The full dev\n\
         set and change cone are unchanged; wait is the default.\n\n\
         GitHub reads and writes use agh (dry-run, expected refs, durable write intents); local\n\
         remotes use git's file protocol only. Required submodules must already be initialised\n\
         and their objects imported: landing never clones or fetches them implicitly.\n\n\
         Exit: 0 landed (or nothing to land), 1 a red or a conflict (nothing pushed), 2 usage,\n\
         3 could not run, {STOPPED} stopped while {MAIN} kept moving (merged and judged, not pushed).\n"
    )
}

fn admission(args: &[String]) -> Option<aterm_capacity::Admit> {
    match args {
        [] => Some(aterm_capacity::Admit::Wait),
        [flag, value] if flag == aterm_capacity::Admit::FLAG => aterm_capacity::Admit::parse(value),
        [flag] => flag
            .strip_prefix("--admit=")
            .and_then(aterm_capacity::Admit::parse),
        _ => None,
    }
}

/// `verify land [--admit wait|now] [--help]`, from `cwd`'s checkout.
pub fn run(args: &[String], cwd: &Path) -> i32 {
    match args {
        [help] if help == "--help" || help == "-h" => {
            print!("{}", usage());
            return exit::PASS;
        }
        _ => {}
    }
    let admit = match admission(args) {
        Some(admit) => admit,
        None => {
            eprint!("verify land: takes only --admit wait|now\n{}", usage());
            return exit::USAGE;
        }
    };
    let Some(root) = crate::checkout_root(cwd) else {
        eprintln!(
            "verify land: COULD NOT RUN — {} is not inside a git checkout",
            cwd.display()
        );
        return exit::COULD_NOT_RUN;
    };
    let git = |root: &Path| {
        let mut cmd = crate::judged::command(root, &[]);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd
    };
    let mut say = |line: &str| println!("{line}");
    land(
        &root,
        &mut Seams {
            git: &git,
            judge: &mut |root, base| live_judge(root, base, admit),
            branch: &mut live_branch,
            lane: &mut live_lane,
            say: &mut say,
        },
    )
}

/// The dev set over the cone since `base`, run as the checkout's own verify (`targo
/// --unverified verify --base <base>`, so the merged tree's tools judge it), its rows read from
/// the check-finished events it keeps and its record's end from its run-finished one.
fn live_judge(root: &Path, base: &str, admit: aterm_capacity::Admit) -> Judged {
    let began = Instant::now();
    let not_run = |why: String| {
        println!("verify land: {why}");
        Judged {
            code: exit::COULD_NOT_RUN,
            ..Judged::default()
        }
    };
    let env = crate::EnvSnapshot::capture();
    let tools = match crate::preflight(&env, root) {
        Ok(tools) => tools,
        Err(why) => return not_run(why),
    };
    let dir = match aterm_tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => return not_run(format!("cannot make a scratch directory: {e}")),
    };
    let events = dir.path().join("verify.events");
    // This is the native driver, bypassing atpkg's shim. Place its bootstrap too:
    // main_targo only places children after the nested verifier has already compiled.
    let build_dir = crate::main_build_dir(&env, root)
        .or_else(|| crate::fallback_build_dir(&env, root, &crate::physical_target(&env, root)));
    let mut cmd = judgment_command(
        &tools.targo,
        root,
        base,
        &events,
        admit,
        build_dir.as_deref(),
    );
    let code = match cmd.status() {
        Ok(status) => status.code().unwrap_or(exit::COULD_NOT_RUN),
        Err(e) => return not_run(format!("{} did not start: {e}", tools.targo.display())),
    };
    let text = std::fs::read_to_string(&events).unwrap_or_default();
    Judged::from_events(&text, code, began.elapsed().as_secs())
}

fn judgment_command(
    targo: &Path,
    root: &Path,
    base: &str,
    events: &Path,
    admit: aterm_capacity::Admit,
    build_dir: Option<&str>,
) -> Command {
    let mut cmd = aterm_uds::spawnfd::command(targo);
    if let Some(value) = build_dir {
        cmd.args(["--config", value]);
    }
    cmd.current_dir(root)
        .args(["--unverified", "verify", "--base", base, "--events"])
        .arg(events)
        .args([aterm_capacity::Admit::FLAG, admit.word()]);
    cmd
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

#[path = "land_changelog.rs"]
mod changelog;

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;
