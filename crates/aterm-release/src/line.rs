// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The candidate line (A18 of `docs/DESIGN-fast-build-release-2026-11-05.md`, decision 4): a
//! release is one base G on origin/main plus hops H1…Hk, each the patch of one landed, non-merge
//! main commit carried onto the previous head with git plumbing alone. A fix after a red is a
//! hop, so the release never re-selects a moving main.
//!
//! * [`admit`] decides whether a commit can be carried: a non-merge commit of origin/main that
//!   the line does not hold yet, moving no [`FROZEN`] path and not the release version.
//! * [`order`] puts the admitted fixes in ancestry order. [`carry`] makes one hop per fix
//!   ([`hop_tree`]'s `git merge-tree` onto the head, then `git commit-tree` under the carried
//!   commit's own author and committer, so a rebuild makes the same commit).
//! * [`record`] puts the head into main's history with a content-neutral merge (main's own tree,
//!   parents main's tip and the head), pushed fast-forward and rebuilt on a moved tip: the engine
//!   stages only commits of main, and main's bytes do not change.
//! * [`verify`] is the meaning check, "the base is on main and each hop's patch is a landed main
//!   commit": every hop recomputes from its carried commit onto the previous head.
//!
//! A landed commit is one origin/main brought into its own tree ([`landed_since`]): a record
//! merge puts a hop into main's history, and no hop's content into main, so a commit main
//! reaches only through such a content-neutral merge is never carried.
//!
//! Every function reads this clone's `origin/main` as last fetched, except [`record`], which
//! fetches. A [`Stop`]'s [`Reason`] says what stopped it, and whether the line was judged (a
//! refusal) or not (COULD NOT RUN: a git too old, the network, an object this clone lacks, a
//! merge driver this clone's own config picks).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ledger::{self, GitRunner, RELEASE_COMMAND, RunOut};

/// The paths a hop never moves: the Trust lock (decision 10), the toolchain pin until that lock
/// replaces it, and the build-number ledger, which only a claim appends to.
pub const FROZEN: [&str; 3] = ["trust.lock", "rust-toolchain.toml", ledger::LEDGER_FILE];

/// The manifest whose `[workspace.package] version` is the release a line ships; no hop moves it.
const MANIFEST: &str = "Cargo.toml";

/// The pushes [`record`] tries before it stops: the first one plus four rebuilds on a moved tip.
pub const MAX_RECORD_ATTEMPTS: u32 = 5;

/// The oldest git whose `merge-tree` a line runs: `--merge-base` came in 2.40, `-X` in 2.43.
pub const MIN_GIT: (u32, u32) = (2, 43);

/// How many of one merge side's commits a refusal lists before it gives only their count.
const LISTED: usize = 20;

/// One release line: its version, its base G and the hops carried onto it. The head ships.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// The release version, `X.Y.0`: the base's `[workspace.package] version`, which no hop moves.
    pub version: String,
    /// G, full: the commit of origin/main the line started at.
    pub base: String,
    /// Epoch seconds the line started, by the clock of the run that chose its base.
    #[serde(default)]
    pub started: u64,
    /// H1…Hk in order. Each one's single parent is the one before it, and G is H1's.
    #[serde(default)]
    pub hops: Vec<Hop>,
}

/// One hop: a commit whose single parent is the previous head and whose tree is that head's with
/// one landed fix's patch.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    /// H, full.
    pub commit: String,
    /// The landed non-merge commit of origin/main whose patch H carries, full.
    pub carries: String,
    /// That commit's subject.
    pub why: String,
    /// The paths H moves against the previous head.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Line {
    /// A line at `base` with no hops yet.
    #[must_use]
    pub fn start(version: &str, base: &str, started: u64) -> Self {
        Self {
            version: version.to_string(),
            base: base.to_string(),
            started,
            hops: Vec::new(),
        }
    }

    /// The commit the line ships: its last hop, or the base before any hop.
    #[must_use]
    pub fn head(&self) -> &str {
        self.hops
            .last()
            .map_or(self.base.as_str(), |hop| hop.commit.as_str())
    }

    /// One of the hops carries `commit` (full).
    #[must_use]
    pub fn carried(&self, commit: &str) -> bool {
        self.hops.iter().any(|hop| hop.carries == commit)
    }
}

/// A commit [`admit`] accepted, which [`carry`] makes a hop of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    /// Full.
    pub commit: String,
    pub subject: String,
    /// The paths it moves against its parent.
    pub paths: Vec<String>,
}

/// What [`admit`] made of a named commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admitted {
    Fix(Fix),
    /// A hop of the line carries it already: naming it again has nothing to do.
    Carried,
}

/// What stopped a line operation. The refusals judge the line or a commit named for it; the
/// reasons [`Reason::could_not_run`] names judge nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// `--fix` names no commit of this clone.
    NoCommit,
    /// A root commit, with no parent to carry a patch from.
    Root,
    /// A merge: a hop carries one non-merge commit.
    Merge,
    /// A commit that is not on origin/main, or not landed there: origin/main reaches it only
    /// through a content-neutral merge (a record merge's, [`landed_since`]).
    OffMain,
    /// The line holds the commit already: the head (for a hop, the previous head) descends from it.
    Held,
    /// A [`FROZEN`] path moves.
    Frozen,
    /// The release version moves: a commit's against its parent, or a hop's against the line's.
    Version,
    /// The patch does not apply onto the head.
    Conflict,
    /// The patch moves nothing: the head has its change.
    Empty,
    /// A commit the line names is not a full sha of a commit object.
    Spelling,
    /// The line's base declares another version than the line's.
    BaseVersion,
    /// The line's base is not on origin/main.
    BaseOffMain,
    /// A hop's parents are not exactly the previous head.
    Parent,
    /// Two hops carry one commit.
    Duplicate,
    /// A hop's tree is not its carried commit's patch onto the previous head.
    Tree,
    /// A hop's recorded paths are not the ones it moves.
    Paths,
    /// The line's head is not on origin/main: its record merge has not landed.
    Unrecorded,
    /// COULD NOT RUN: git cannot read this clone, or the clone lacks an object.
    Unread,
    /// COULD NOT RUN: this git is older than [`MIN_GIT`].
    OldGit,
    /// COULD NOT RUN: this clone's own config or attributes pick a merge driver for a path the
    /// carried commit moves, so the hop's tree would not be the commits' alone ([`hop_tree`]).
    Steered,
    /// COULD NOT RUN: origin cannot be fetched, or a push failed but not by a lost race.
    Unreachable,
    /// COULD NOT RUN: main moved under every record push.
    Raced,
    /// COULD NOT RUN: the record push succeeded and origin/main does not hold the head.
    Unlanded,
}

impl Reason {
    /// Nothing about the line was decided: a resumable stop, never a pass.
    #[must_use]
    pub const fn could_not_run(self) -> bool {
        matches!(
            self,
            Self::Unread
                | Self::OldGit
                | Self::Steered
                | Self::Unreachable
                | Self::Raced
                | Self::Unlanded
        )
    }
}

/// Why a line operation stopped, in the fields of a release refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub reason: Reason,
    pub why: String,
    pub remedy: String,
    /// Lines printed above the final line.
    pub detail: Vec<String>,
}

impl Stop {
    fn new(reason: Reason, why: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            reason,
            why: why.into(),
            remedy: remedy.into(),
            detail: Vec::new(),
        }
    }

    fn detail(mut self, lines: impl IntoIterator<Item = String>) -> Self {
        self.detail.extend(lines);
        self
    }

    /// [`Reason::could_not_run`] of its reason.
    #[must_use]
    pub const fn could_not_run(&self) -> bool {
        self.reason.could_not_run()
    }
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} — {}", self.why, self.remedy)
    }
}

impl From<Stop> for crate::release::Refusal {
    fn from(stop: Stop) -> Self {
        let could_not_run = stop.could_not_run();
        let refusal = Self::new(stop.why, stop.remedy).detail(stop.detail);
        if could_not_run {
            refusal.could_not_run()
        } else {
            refusal
        }
    }
}

/// What carrying one commit onto a head gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merged {
    /// The head's tree with the commit's patch: the next hop's tree.
    Tree(String),
    /// The patch does not apply to the head: the paths that conflict.
    Conflict(Vec<String>),
}

/// What [`record`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// origin/main held the head already (a re-run, or a peer recorded it): nothing was pushed.
    Already { tip: String },
    /// The record merge landed on main, on the push numbered `attempt`.
    Pushed { merge: String, attempt: u32 },
}

/// Whether `rev` can be carried onto `line`, checked in this order: it names a commit; a hop
/// carries it already ([`Admitted::Carried`]); it is not a merge (a refusal lists each side's
/// non-merge commits, for the agent to name); it is on origin/main; the line's head does not
/// hold it; it landed on main ([`landed_since`] the line's base: not only a record merge's
/// second parent, like another line's hop); it moves no [`FROZEN`] path and not the release
/// version.
///
/// # Errors
/// Each refusal above, and COULD NOT RUN when git cannot read this clone.
pub fn admit(git: &dyn GitRunner, line: &Line, rev: &str) -> Result<Admitted, Stop> {
    let named = || {
        Stop::new(
            Reason::NoCommit,
            format!("--fix {rev:?} names no commit of this clone"),
            "fetch origin (`git fetch origin`) and name the landed commit by its sha",
        )
    };
    if rev.is_empty() || rev.starts_with('-') {
        return Err(named());
    }
    let resolved = run(
        git,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )?;
    if !resolved.success() {
        return Err(named());
    }
    let commit = resolved.stdout_utf8().trim().to_string();
    if line.carried(&commit) {
        return Ok(Admitted::Carried);
    }
    let read = read_commit(git, &commit)?;
    let parent = match read.parents.as_slice() {
        [parent] => parent.clone(),
        [] => {
            return Err(Stop::new(
                Reason::Root,
                format!(
                    "{} is a root commit: it has no parent to carry a patch from",
                    short(&commit)
                ),
                "name the landed fix commit",
            ));
        }
        parents => return Err(merge_refusal(git, &commit, parents)?),
    };
    if !is_ancestor(git, &commit, "origin/main")? {
        return Err(Stop::new(
            Reason::OffMain,
            format!(
                "{} ({}) is not on origin/main: a hop carries only a landed fix",
                short(&commit),
                read.subject
            ),
            format!("land it on main first, then `{RELEASE_COMMAND} --fix <its commit>`"),
        ));
    }
    if is_ancestor(git, &commit, line.head())? {
        return Err(Stop::new(
            Reason::Held,
            format!(
                "{} ({}) is already in the line: its head {} holds it",
                short(&commit),
                read.subject,
                short(line.head())
            ),
            "nothing to carry: drop it from --fix",
        ));
    }
    if !landed_since(git, &line.base)?.contains(&commit) {
        return Err(unlanded(&commit, &read.subject));
    }
    let paths = moved(git, &parent, &commit)?;
    let held = frozen(&paths);
    if !held.is_empty() {
        return Err(Stop::new(
            Reason::Frozen,
            format!(
                "{} ({}) moves {}, which a release line never moves",
                short(&commit),
                read.subject,
                held.join(", ")
            ),
            "it ships with the next line, started on main after this release",
        ));
    }
    let (before, after) = (version_at(git, &parent)?, version_at(git, &commit)?);
    if before != after {
        return Err(Stop::new(
            Reason::Version,
            format!(
                "{} ({}) moves the release version ({} → {}): a bump starts a line, never a hop",
                short(&commit),
                read.subject,
                before.as_deref().unwrap_or("none"),
                after.as_deref().unwrap_or("none")
            ),
            "name the fixes alone",
        ));
    }
    Ok(Admitted::Fix(Fix {
        commit,
        subject: read.subject,
        paths,
    }))
}

/// `fixes` in ancestry order (a commit after each of its ancestors among them), one entry per
/// commit. Commits neither of which holds the other go by committer time, then by sha, so the
/// order and the hops it makes never depend on the order the agent named them in.
///
/// # Errors
/// COULD NOT RUN when git cannot read a commit.
pub fn order(git: &dyn GitRunner, fixes: Vec<Fix>) -> Result<Vec<Fix>, Stop> {
    let mut unique: Vec<Fix> = Vec::with_capacity(fixes.len());
    for fix in fixes {
        if unique.iter().all(|f| f.commit != fix.commit) {
            unique.push(fix);
        }
    }
    let n = unique.len();
    let mut before = vec![vec![false; n]; n];
    for (i, row) in before.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            if i != j {
                *cell = is_ancestor(git, &unique[i].commit, &unique[j].commit)?;
            }
        }
    }
    let mut times = Vec::with_capacity(n);
    for fix in &unique {
        times.push(read_commit(git, &fix.commit)?.committer.time);
    }
    let mut placed = vec![false; n];
    let mut sequence = Vec::with_capacity(n);
    while sequence.len() < n {
        let next = (0..n)
            .filter(|&j| !placed[j] && (0..n).all(|i| placed[i] || !before[i][j]))
            .min_by(|&a, &b| (times[a], &unique[a].commit).cmp(&(times[b], &unique[b].commit)))
            .ok_or_else(|| unread("the fixes' ancestry has a cycle, which git history cannot"))?;
        placed[next] = true;
        sequence.push(next);
    }
    let mut slots: Vec<Option<Fix>> = unique.into_iter().map(Some).collect();
    Ok(sequence
        .into_iter()
        .filter_map(|i| slots[i].take())
        .collect())
}

/// The tree of `c`'s patch (against its first parent) applied onto `prev` by git's default
/// three-way text merge, `git merge-tree --write-tree --merge-base c^ prev c`, as a function of
/// the three commits alone: rename detection off (`-X no-renames`), no renormalizing and the
/// histogram diff whatever this clone's config says (`-X no-renormalize -X
/// diff-algorithm=histogram`), and attributes read from the empty tree with no global attributes
/// file (`--attr-source`, `-c core.attributesFile=/dev/null`), never from the checkout's
/// `.gitattributes`. The sources those pins cannot reach must pick no merge driver for a path `c`
/// moves: `merge.default` is unset, and neither `$GIT_DIR/info/attributes` nor the system
/// gitattributes give such a path a `merge` attribute.
///
/// # Errors
/// COULD NOT RUN when one of those sources picks a driver ([`Reason::Steered`]), when git refuses
/// a call (a git older than [`MIN_GIT`] does, exit 129) or cannot read the commits.
pub fn hop_tree(git: &dyn GitRunner, prev: &str, c: &str) -> Result<Merged, Stop> {
    let base = format!("{c}^");
    let empty = plumb(git, &["hash-object", "-w", "-t", "tree", "/dev/null"])?;
    let source = format!("--attr-source={}", empty.stdout_utf8().trim());
    let pins = [source.as_str(), "-c", "core.attributesFile=/dev/null"];
    unsteered(git, &pins, &base, c)?;
    let args: Vec<&str> = pins
        .iter()
        .copied()
        .chain([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            "-X",
            "no-renames",
            "-X",
            "no-renormalize",
            "-X",
            "diff-algorithm=histogram",
            "--merge-base",
            &base,
            prev,
            c,
        ])
        .collect();
    let merged = call(git, &args)?;
    let mut fields = merged.stdout.split(|b| *b == 0).map(lossy);
    let tree = fields.next().unwrap_or_default();
    match merged.status {
        0 if !tree.is_empty() => Ok(Merged::Tree(tree)),
        1 => Ok(Merged::Conflict(
            fields.take_while(|path| !path.is_empty()).collect(),
        )),
        _ => Err(refused_call(git, &args, &merged)),
    }
}

/// Nothing this clone holds outside its objects picks a merge driver for a path `c` moves
/// against `base`: `merge.default` is unset, and under `pins` (which leave the system
/// gitattributes and `$GIT_DIR/info/attributes`) no such path has a `merge` attribute. Only a
/// path both sides move reaches a driver, and `c` moves each of those.
fn unsteered(git: &dyn GitRunner, pins: &[&str], base: &str, c: &str) -> Result<(), Stop> {
    let mut steering = Vec::new();
    let default_args = ["config", "--get", "merge.default"];
    let default = call(git, &default_args)?;
    match default.status {
        0 => steering.push(format!(
            "merge.default = {} picks the driver of every path without a merge attribute",
            default.stdout_utf8().trim()
        )),
        1 => {}
        _ => return Err(refused_call(git, &default_args, &default)),
    }
    let listed = plumb(
        git,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            base,
            c,
        ],
    )?;
    if !listed.stdout.is_empty() {
        let args: Vec<&str> = pins
            .iter()
            .copied()
            .chain(["check-attr", "-z", "--stdin", "merge"])
            .collect();
        let attrs = git
            .git_input(&args, &listed.stdout)
            .map_err(|e| unread(e.to_string()))?;
        if !attrs.success() {
            return Err(refused_call(git, &args, &attrs));
        }
        let fields: Vec<std::borrow::Cow<'_, str>> = attrs
            .stdout
            .split(|b| *b == 0)
            .map(String::from_utf8_lossy)
            .collect();
        for triple in fields.chunks(3) {
            if let [path, _, value] = triple
                && value != "unspecified"
            {
                steering.push(format!("{path}: merge {value}"));
            }
        }
    }
    if steering.is_empty() {
        return Ok(());
    }
    Err(Stop::new(
        Reason::Steered,
        format!(
            "this clone's own config or attributes pick a merge driver for what {} moves, so a \
             hop's tree would not be the commits' alone",
            short(c)
        ),
        "remove them (`git config --show-origin merge.default`, the file `git rev-parse \
         --git-path info/attributes` names, the system gitattributes), then run the release again",
    )
    .detail(steering))
}

/// One hop per fix, in the order given ([`order`]'s), onto the line's head: each tree is
/// [`hop_tree`]'s onto the previous head, committed with that head as its one parent, the
/// message `release line v<version> hop <k>: carries <commit> (<subject>)`, and the carried
/// commit's author and committer (names, emails and dates), unsigned: a rebuild after a crash
/// makes the same commits. Nothing is pushed; [`record`] puts the head on main.
///
/// # Errors
/// A refusal for a fix not named by its full sha, one that conflicts with the previous head (the
/// remedy: author it on the line's head and land it, or, when fixes of this run come before it,
/// carry those first and author it on the head that makes), one that carries nothing (the head
/// has its change), and a hop that would move a [`FROZEN`] path or the release version; COULD
/// NOT RUN when git cannot run the plumbing or [`hop_tree`] cannot.
pub fn carry(git: &dyn GitRunner, line: &Line, fixes: &[Fix]) -> Result<Vec<Hop>, Stop> {
    let head = line.head();
    let mut prev = head.to_string();
    let mut prev_tree = tree_of(git, &prev)?;
    let mut hops: Vec<Hop> = Vec::with_capacity(fixes.len());
    for fix in fixes {
        let k = line.hops.len() + hops.len() + 1;
        if !is_full_sha(&fix.commit) {
            return Err(Stop::new(
                Reason::Spelling,
                format!("the fix {:?} is not a full commit sha", fix.commit),
                "carry the fixes `admit` gives",
            ));
        }
        let earlier: Vec<&str> = hops.iter().map(|hop| short(&hop.carries)).collect();
        let before: Vec<String> = earlier
            .iter()
            .map(|sha| format!("carried before it in this run: {sha}"))
            .collect();
        let tree = match hop_tree(git, &prev, &fix.commit)? {
            Merged::Tree(tree) => tree,
            Merged::Conflict(paths) => {
                let remedy = if earlier.is_empty() {
                    format!(
                        "author it on {} and land it, then `{RELEASE_COMMAND} --fix <that commit>`",
                        short(head)
                    )
                } else {
                    format!(
                        "carry the fixes before it first (`{RELEASE_COMMAND} --fix {}`), then \
                         author it on the head that makes and land it, then `{RELEASE_COMMAND} \
                         --fix <that commit>`",
                        earlier.join(" ")
                    )
                };
                return Err(Stop::new(
                    Reason::Conflict,
                    format!(
                        "{} ({}) does not carry onto the line's head: it conflicts in {}",
                        short(&fix.commit),
                        fix.subject,
                        paths.join(", ")
                    ),
                    remedy,
                )
                .detail(before));
            }
        };
        if tree == prev_tree {
            return Err(Stop::new(
                Reason::Empty,
                format!(
                    "{} ({}) carries nothing: the line's head has its change already",
                    short(&fix.commit),
                    fix.subject
                ),
                "drop it from --fix",
            )
            .detail(before));
        }
        let source = read_commit(git, &fix.commit)?;
        let message = format!(
            "release line v{} hop {k}: carries {} ({})",
            line.version, fix.commit, fix.subject
        );
        let args = [
            "-c",
            "i18n.commitEncoding=UTF-8",
            "commit-tree",
            "--no-gpg-sign",
            &tree,
            "-p",
            &prev,
            "-m",
            &message,
        ];
        let made = git
            .git_env(&args, &source.pinned())
            .map_err(|e| unread(e.to_string()))?;
        if !made.success() {
            return Err(unread(format!(
                "git commit-tree for hop {k} failed (exit {}): {}",
                made.status,
                made.stderr_utf8().trim()
            )));
        }
        let commit = made.stdout_utf8().trim().to_string();
        let paths = moved(git, &prev, &commit)?;
        holds_still(git, line, k, &commit, &paths)?;
        hops.push(Hop {
            commit: commit.clone(),
            carries: fix.commit.clone(),
            why: fix.subject.clone(),
            paths,
        });
        prev = commit;
        prev_tree = tree;
    }
    Ok(hops)
}

/// Put `head` into origin/main's history without changing main's bytes: fetch; when main holds
/// `head` already, nothing; else the merge `commit-tree <tip>^{tree} -p <tip> -p <head>`
/// ("release line v<version>: record <head> on main (main's tree unchanged)") pushed
/// fast-forward to main. A push that lost the race to a peer ([`ledger::lost_race`], here or at
/// the remote) is rebuilt on the new tip, up to `max` pushes in all; a push that landed is
/// checked by a fresh fetch.
///
/// # Errors
/// COULD NOT RUN for an origin that cannot be fetched or pushed, `max` lost races, and a pushed
/// merge that origin/main does not hold afterwards: in each, a re-run records it.
pub fn record(git: &dyn GitRunner, version: &str, head: &str, max: u32) -> Result<Recorded, Stop> {
    let again = "run the release again, which records the line's head on the tip it finds";
    let mut attempt = 0_u32;
    loop {
        attempt += 1;
        fetch(git)?;
        let tip = out(git, &["rev-parse", "origin/main"])?;
        if is_ancestor(git, head, &tip)? {
            return Ok(Recorded::Already { tip });
        }
        let message = format!(
            "release line v{version}: record {} on main (main's tree unchanged)",
            short(head)
        );
        let tree = format!("{tip}^{{tree}}");
        let merge = out(
            git,
            &["commit-tree", &tree, "-p", &tip, "-p", head, "-m", &message],
        )?;
        let push = run(
            git,
            &["push", "origin", &format!("{merge}:refs/heads/main")],
        )?;
        if push.success() {
            fetch(git)?;
            if !is_ancestor(git, head, "origin/main")? {
                return Err(Stop::new(
                    Reason::Unlanded,
                    format!(
                        "the push of the record merge {} succeeded, and origin/main does not hold \
                         the line's head {}",
                        short(&merge),
                        short(head)
                    ),
                    again,
                ));
            }
            return Ok(Recorded::Pushed { merge, attempt });
        }
        let why = push.stderr_utf8().trim().to_string();
        if !ledger::lost_race(&why) {
            return Err(Stop::new(
                Reason::Unreachable,
                format!(
                    "git push origin main failed (not a lost race, so retrying cannot help): {why}"
                ),
                "check the network and your git credentials, then run the release again",
            ));
        }
        if attempt >= max {
            return Err(Stop::new(
                Reason::Raced,
                format!("main moved under the record push {attempt} times; last rejection: {why}"),
                again,
            ));
        }
    }
}

/// The meaning check, "the base is on main and each hop's patch is a landed main commit": the
/// base is a commit this clone holds, named by its full sha, that declares the line's version and
/// is on origin/main; each hop is such a commit whose one parent is the previous head, it
/// carries, by its full sha, a non-merge commit of origin/main that no earlier hop carries and
/// the previous head does not descend from, landed on main ([`landed_since`] the base), its
/// tree is [`hop_tree`]'s for that commit onto the previous head and differs from it, its
/// recorded paths are the ones it moves, none of them [`FROZEN`], and it declares the line's
/// version; the head is on origin/main (its record merge landed). Ok is a one-line summary.
///
/// # Errors
/// A refusal naming the first mismatch, in that order; COULD NOT RUN when git cannot read an
/// object, the clone lacks one, or [`hop_tree`] cannot run.
pub fn verify(git: &dyn GitRunner, line: &Line) -> Result<String, Stop> {
    let bad = |reason: Reason, why: String| Stop::new(reason, why, drop_line());
    let judged = |stop: Stop| {
        if stop.could_not_run() {
            stop
        } else {
            bad(stop.reason, stop.why)
        }
    };
    named(git, &line.base, "the base")?;
    let declared = version_at(git, &line.base)?;
    if declared.as_deref() != Some(line.version.as_str()) {
        return Err(bad(
            Reason::BaseVersion,
            format!(
                "the base {} declares version {}, not the line's {}",
                short(&line.base),
                declared.as_deref().unwrap_or("none"),
                line.version
            ),
        ));
    }
    if !is_ancestor(git, &line.base, "origin/main")? {
        return Err(bad(
            Reason::BaseOffMain,
            format!("the base {} is not on origin/main", short(&line.base)),
        ));
    }
    let mut prev = line.base.clone();
    let mut prev_tree = tree_of(git, &prev)?;
    let mut landed = None;
    for (i, hop) in line.hops.iter().enumerate() {
        let k = i + 1;
        named(git, &hop.commit, &format!("hop {k}"))?;
        let made = read_commit(git, &hop.commit)?;
        if made.parents != [prev.as_str()] {
            return Err(bad(
                Reason::Parent,
                format!(
                    "hop {k} ({}) has the parents [{}]; its one parent must be the previous head {}",
                    short(&hop.commit),
                    made.parents
                        .iter()
                        .map(|p| short(p))
                        .collect::<Vec<_>>()
                        .join(", "),
                    short(&prev)
                ),
            ));
        }
        named(git, &hop.carries, &format!("hop {k}'s carried commit"))?;
        if line.hops[..i].iter().any(|h| h.carries == hop.carries) {
            return Err(bad(
                Reason::Duplicate,
                format!("hop {k} carries {} again", short(&hop.carries)),
            ));
        }
        let carried = read_commit(git, &hop.carries)?;
        match carried.parents.len() {
            1 => {}
            0 => {
                return Err(bad(
                    Reason::Root,
                    format!("hop {k} carries {}, a root commit", short(&hop.carries)),
                ));
            }
            _ => {
                return Err(bad(
                    Reason::Merge,
                    format!(
                        "hop {k} carries {}, which is not a non-merge commit",
                        short(&hop.carries)
                    ),
                ));
            }
        }
        if !is_ancestor(git, &hop.carries, "origin/main")? {
            return Err(bad(
                Reason::OffMain,
                format!(
                    "hop {k} carries {}, which is not on origin/main",
                    short(&hop.carries)
                ),
            ));
        }
        if is_ancestor(git, &hop.carries, &prev)? {
            return Err(bad(
                Reason::Held,
                format!(
                    "hop {k} carries {}, which the previous head {} descends from: its patch is in \
                     the line already",
                    short(&hop.carries),
                    short(&prev)
                ),
            ));
        }
        if landed.is_none() {
            landed = Some(landed_since(git, &line.base)?);
        }
        if !landed
            .as_ref()
            .is_some_and(|set| set.contains(&hop.carries))
        {
            let stop = unlanded(&hop.carries, &carried.subject);
            return Err(bad(stop.reason, format!("hop {k}: {}", stop.why)));
        }
        let tree = tree_of(git, &hop.commit)?;
        match hop_tree(git, &prev, &hop.carries)? {
            Merged::Tree(carried) if carried == tree => {}
            Merged::Tree(carried) => {
                return Err(bad(
                    Reason::Tree,
                    format!(
                        "hop {k}'s tree {} is not {}'s patch onto the previous head (that is {})",
                        short(&tree),
                        short(&hop.carries),
                        short(&carried)
                    ),
                ));
            }
            Merged::Conflict(paths) => {
                return Err(bad(
                    Reason::Conflict,
                    format!(
                        "hop {k}'s carried {} does not apply onto the previous head (conflicts in \
                         {})",
                        short(&hop.carries),
                        paths.join(", ")
                    ),
                ));
            }
        }
        if tree == prev_tree {
            return Err(bad(
                Reason::Empty,
                format!("hop {k} ({}) moves nothing", short(&hop.commit)),
            ));
        }
        let paths = moved(git, &prev, &hop.commit)?;
        if paths != hop.paths {
            return Err(bad(
                Reason::Paths,
                format!(
                    "hop {k} records the paths [{}] and moves [{}]",
                    hop.paths.join(", "),
                    paths.join(", ")
                ),
            ));
        }
        holds_still(git, line, k, &hop.commit, &paths).map_err(judged)?;
        prev = hop.commit.clone();
        prev_tree = tree;
    }
    if !is_ancestor(git, &prev, "origin/main")? {
        return Err(Stop::new(
            Reason::Unrecorded,
            format!(
                "the line's head {} is not on origin/main: origin/main does not hold its record \
                 merge",
                short(&prev)
            ),
            drop_line(),
        ));
    }
    let carried: Vec<&str> = line.hops.iter().map(|hop| short(&hop.carries)).collect();
    Ok(if carried.is_empty() {
        format!(
            "line v{}: its base {} is on origin/main, with no hops",
            line.version,
            short(&line.base)
        )
    } else {
        format!(
            "line v{}: base {} on origin/main, {} hop(s), each a landed main commit's patch \
             onto the previous head ({}); head {} is on origin/main",
            line.version,
            short(&line.base),
            carried.len(),
            carried.join(", "),
            short(&prev)
        )
    })
}

/// [`verify`]'s remedy for a line it judged.
fn drop_line() -> String {
    format!("this line cannot ship: drop it (`{RELEASE_COMMAND} --forget`) and start a new one")
}

/// The commits origin/main brought into its own tree since `floor`: those its tip reaches and
/// `floor` does not, walking every parent except past a content-neutral merge (one whose tree is
/// its first parent's, as each [`record`] merge is), where only the first parent is followed. A
/// record merge puts a hop into main's history and none of its content into main's tree, so a
/// commit main reaches only that way (another line's hop, or anything pushed under such a merge)
/// is in origin/main's history and not in this set.
///
/// # Errors
/// COULD NOT RUN when git cannot list or read the history.
pub fn landed_since(git: &dyn GitRunner, floor: &str) -> Result<BTreeSet<String>, Stop> {
    let tip = out(git, &["rev-parse", "origin/main^{commit}"])?;
    let listed = out(
        git,
        &[
            "rev-list",
            "--no-commit-header",
            "--format=%H %T %P",
            "origin/main",
            &format!("^{floor}"),
        ],
    )?;
    let mut range: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for row in listed.lines() {
        let mut words = row.split_whitespace().map(str::to_string);
        let (Some(commit), Some(tree)) = (words.next(), words.next()) else {
            return Err(unread(format!(
                "git rev-list wrote {row:?}, not `<commit> <tree> <parents>`"
            )));
        };
        range.insert(commit, (tree, words.collect()));
    }
    let mut trees: BTreeMap<String, String> = range
        .iter()
        .map(|(commit, (tree, _))| (commit.clone(), tree.clone()))
        .collect();
    let outside: BTreeSet<String> = range
        .values()
        .filter(|(_, parents)| parents.len() > 1)
        .filter_map(|(_, parents)| parents.first())
        .filter(|first| !trees.contains_key(*first))
        .cloned()
        .collect();
    if !outside.is_empty() {
        let spelled: Vec<String> = outside.iter().map(|c| format!("{c}^{{tree}}")).collect();
        let mut args = vec!["rev-parse"];
        args.extend(spelled.iter().map(String::as_str));
        let read = out(git, &args)?;
        let read: Vec<&str> = read.lines().collect();
        if read.len() != outside.len() {
            return Err(unread(format!(
                "git rev-parse named {} tree(s) for {} first parent(s)",
                read.len(),
                outside.len()
            )));
        }
        for (commit, tree) in outside.into_iter().zip(read) {
            trees.insert(commit, tree.to_string());
        }
    }
    let mut landed = BTreeSet::new();
    let mut next = vec![tip];
    while let Some(commit) = next.pop() {
        let Some((tree, parents)) = range.get(&commit) else {
            continue;
        };
        if !landed.insert(commit) {
            continue;
        }
        let neutral = parents.len() > 1
            && parents
                .first()
                .and_then(|first| trees.get(first))
                .is_some_and(|first| first == tree);
        let followed = if neutral { &parents[..1] } else { &parents[..] };
        next.extend(followed.iter().filter(|p| !landed.contains(*p)).cloned());
    }
    Ok(landed)
}

/// The refusal for a commit origin/main holds only through a content-neutral merge.
fn unlanded(commit: &str, subject: &str) -> Stop {
    Stop::new(
        Reason::OffMain,
        format!(
            "{} ({subject}) is in origin/main's history only through a merge that keeps its first \
             parent's tree (a release line's record merge, or one like it): its change never \
             landed in main's tree, and a hop carries only a landed fix",
            short(commit)
        ),
        format!(
            "name the main commit that landed the change (`git log origin/main`), or land it on \
             main first, then `{RELEASE_COMMAND} --fix <its commit>`"
        ),
    )
}

/// `rev`'s tree holds the file `path` (a blob or a symlink at exactly that path, never a
/// directory there).
///
/// # Errors
/// COULD NOT RUN when this clone lacks `rev` or git cannot read its tree.
pub fn tree_has(git: &dyn GitRunner, rev: &str, path: &str) -> Result<bool, Stop> {
    if !run(git, &["cat-file", "-e", &format!("{rev}^{{commit}}")])?.success() {
        return Err(lacks("commit", rev));
    }
    let listed = plumb(git, &["ls-tree", "-z", "--full-tree", rev, "--", path])?;
    Ok(listed.stdout.split(|b| *b == 0).any(|entry| {
        let entry = String::from_utf8_lossy(entry);
        entry
            .split_once('\t')
            .is_some_and(|(meta, name)| name == path && meta.split(' ').nth(1) == Some("blob"))
    }))
}

/// `sha` is a full sha of a commit this clone holds (`role` names it in a stop).
///
/// # Errors
/// A [`Reason::Spelling`] refusal for another spelling or an object that is no commit; COULD NOT
/// RUN for an object this clone lacks.
fn named(git: &dyn GitRunner, sha: &str, role: &str) -> Result<(), Stop> {
    if !is_full_sha(sha) {
        return Err(Stop::new(
            Reason::Spelling,
            format!("{role} {sha:?} is not a full commit sha"),
            drop_line(),
        ));
    }
    let kind = run(git, &["cat-file", "-t", sha])?;
    if !kind.success() {
        return Err(lacks(role, sha));
    }
    match kind.stdout_utf8().trim() {
        "commit" => Ok(()),
        other => Err(Stop::new(
            Reason::Spelling,
            format!("{role} {} names a {other}, not a commit", short(sha)),
            drop_line(),
        )),
    }
}

/// 40 (SHA-1) or 64 (SHA-256) lowercase hex digits: a sha as git writes it, which no ref, tag or
/// abbreviation spells.
fn is_full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn lacks(role: &str, sha: &str) -> Stop {
    Stop::new(
        Reason::Unread,
        format!("this clone lacks {role} {}", short(sha)),
        "fetch origin (`git fetch origin`), then run the release again",
    )
}

/// This clone's git, when it is new enough for a line ([`MIN_GIT`]): its `git version` words.
///
/// # Errors
/// COULD NOT RUN for an older git, or one whose version cannot be read.
pub fn git_ready(git: &dyn GitRunner) -> Result<String, Stop> {
    let words = out(git, &["version"])?;
    match parse_git_version(&words) {
        Some(found) if found >= MIN_GIT => Ok(words),
        _ => Err(too_old(&words)),
    }
}

/// `git version`'s major and minor numbers ("git version 2.39.5 (Apple Git-154)" is (2, 39)).
#[must_use]
pub fn parse_git_version(words: &str) -> Option<(u32, u32)> {
    let number = words
        .trim()
        .strip_prefix("git version ")?
        .split_whitespace()
        .next()?;
    let mut parts = number.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn too_old(words: &str) -> Stop {
    Stop::new(
        Reason::OldGit,
        format!(
            "a release line needs git ≥ {}.{} (`git merge-tree --merge-base` came in 2.40, its \
             `-X no-renames` in 2.43), and this git is {:?}",
            MIN_GIT.0,
            MIN_GIT.1,
            words.trim()
        ),
        format!(
            "put a git ≥ {}.{} first on PATH, then run the release again",
            MIN_GIT.0, MIN_GIT.1
        ),
    )
}

/// `git <args>` as [`run`] gives it, except that a usage refusal (exit 129, as a git older than
/// [`MIN_GIT`] meets an option it lacks) is [`refused_call`]'s stop.
fn call(git: &dyn GitRunner, args: &[&str]) -> Result<RunOut, Stop> {
    let ran = run(git, args)?;
    if ran.status == 129 {
        return Err(refused_call(git, args, &ran));
    }
    Ok(ran)
}

/// [`call`], required to succeed.
fn plumb(git: &dyn GitRunner, args: &[&str]) -> Result<RunOut, Stop> {
    let ran = call(git, args)?;
    if !ran.success() {
        return Err(refused_call(git, args, &ran));
    }
    Ok(ran)
}

/// The stop for a git call that failed: COULD NOT RUN naming [`MIN_GIT`] and the git found when
/// that git is older or its version cannot be read, else COULD NOT RUN with git's own words.
fn refused_call(git: &dyn GitRunner, args: &[&str], ran: &RunOut) -> Stop {
    let words = out(git, &["version"]).unwrap_or_default();
    if parse_git_version(&words).is_none_or(|found| found < MIN_GIT) {
        return too_old(&words);
    }
    let command = args
        .iter()
        .find(|arg| !arg.starts_with('-') && !arg.contains('='))
        .unwrap_or(&"");
    unread(format!(
        "git {command} failed (exit {}): {}",
        ran.status,
        ran.stderr_utf8().trim()
    ))
}

/// The merge refusal: each parent's side, as the non-merge commits the merge has and that
/// parent lacks. For a landing (`Merge origin/main into <branch>`, pushed fast-forward), the
/// branch's own commits are the ones its parent 2, main's tip then, lacks.
fn merge_refusal(git: &dyn GitRunner, merge: &str, parents: &[String]) -> Result<Stop, Stop> {
    let mut detail = Vec::new();
    for (i, parent) in parents.iter().enumerate() {
        let side = out(
            git,
            &[
                "rev-list",
                "--no-merges",
                "--reverse",
                "--no-commit-header",
                "--format=%H %s",
                &format!("{parent}..{merge}"),
            ],
        )?;
        let commits: Vec<&str> = side.lines().filter(|l| !l.is_empty()).collect();
        detail.push(format!(
            "{} non-merge commit(s) it has and its parent {} ({}) lacks:",
            commits.len(),
            i + 1,
            short(parent)
        ));
        detail.extend(commits.iter().take(LISTED).map(|c| format!("  {c}")));
        if commits.len() > LISTED {
            detail.push(format!("  … and {} more", commits.len() - LISTED));
        }
    }
    Ok(Stop::new(
        Reason::Merge,
        format!(
            "{} is a merge: a hop carries one non-merge commit, and a merge's diff against a \
             parent carries the other side's history with it",
            short(merge)
        ),
        format!(
            "name the landing's own commits, the ones the parent that was main's tip lacks (parent \
             2 of a `Merge origin/main into <branch>` landing): `{RELEASE_COMMAND} --fix \
             <commit>…`"
        ),
    )
    .detail(detail))
}

/// A hop moves no [`FROZEN`] path and declares the line's version.
fn holds_still(
    git: &dyn GitRunner,
    line: &Line,
    k: usize,
    hop: &str,
    paths: &[String],
) -> Result<(), Stop> {
    let held = frozen(paths);
    if !held.is_empty() {
        return Err(Stop::new(
            Reason::Frozen,
            format!(
                "hop {k} ({}) moves {}, which a release line never moves",
                short(hop),
                held.join(", ")
            ),
            "it ships with the next line, started on main after this release",
        ));
    }
    let declared = version_at(git, hop)?;
    if declared.as_deref() != Some(line.version.as_str()) {
        return Err(Stop::new(
            Reason::Version,
            format!(
                "hop {k} ({}) declares version {}, not the line's {}",
                short(hop),
                declared.as_deref().unwrap_or("none"),
                line.version
            ),
            "name the fixes alone: a bump starts a line, never a hop",
        ));
    }
    Ok(())
}

/// One commit object as a hop reads it.
struct Commit {
    parents: Vec<String>,
    author: Ident,
    committer: Ident,
    /// Its message's first paragraph on one line, as git's `%s` gives it.
    subject: String,
}

/// An author or committer line: `<name> <<email>> <epoch> <zone>`.
struct Ident {
    name: String,
    email: String,
    /// `<epoch> <zone>`, as git writes it and reads it back from `GIT_*_DATE`.
    date: String,
    time: u64,
}

impl Ident {
    fn parse(text: &str) -> Option<Self> {
        let (who, date) = text.rsplit_once("> ")?;
        let (name, email) = who
            .rsplit_once(" <")
            .or_else(|| who.strip_prefix('<').map(|email| ("", email)))?;
        let time = date.split_whitespace().next()?.parse().ok()?;
        Some(Self {
            name: name.to_string(),
            email: email.to_string(),
            date: date.to_string(),
            time,
        })
    }
}

impl Commit {
    /// The environment that makes `commit-tree` write this commit's author and committer.
    fn pinned(&self) -> [(&str, &str); 6] {
        [
            ("GIT_AUTHOR_NAME", &self.author.name),
            ("GIT_AUTHOR_EMAIL", &self.author.email),
            ("GIT_AUTHOR_DATE", &self.author.date),
            ("GIT_COMMITTER_NAME", &self.committer.name),
            ("GIT_COMMITTER_EMAIL", &self.committer.email),
            ("GIT_COMMITTER_DATE", &self.committer.date),
        ]
    }
}

fn read_commit(git: &dyn GitRunner, sha: &str) -> Result<Commit, Stop> {
    let text = out(git, &["cat-file", "commit", sha])?;
    let (head, message) = text.split_once("\n\n").unwrap_or((text.as_str(), ""));
    let mut parents = Vec::new();
    let (mut author, mut committer) = (None, None);
    for field in head.lines() {
        if let Some(parent) = field.strip_prefix("parent ") {
            parents.push(parent.to_string());
        } else if let Some(who) = field.strip_prefix("author ") {
            author = Ident::parse(who);
        } else if let Some(who) = field.strip_prefix("committer ") {
            committer = Ident::parse(who);
        }
    }
    let subject = message
        .split("\n\n")
        .next()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    match (author, committer) {
        (Some(author), Some(committer)) => Ok(Commit {
            parents,
            author,
            committer,
            subject,
        }),
        _ => Err(unread(format!(
            "commit {sha} has no author or committer line git could write back"
        ))),
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn unread(why: impl Into<String>) -> Stop {
    Stop::new(
        Reason::Unread,
        why,
        "check this clone (`git status`, `git fsck`), then run the release again",
    )
}

fn run(git: &dyn GitRunner, args: &[&str]) -> Result<RunOut, Stop> {
    git.git(args).map_err(|e| unread(e.to_string()))
}

/// `git <args>`, required to succeed: its trimmed stdout.
fn out(git: &dyn GitRunner, args: &[&str]) -> Result<String, Stop> {
    ledger::git_ok(git, args)
        .map(|o| o.stdout_utf8().trim().to_string())
        .map_err(|e| unread(e.to_string()))
}

fn fetch(git: &dyn GitRunner) -> Result<(), Stop> {
    ledger::git_ok(git, &["fetch", "origin", "main"])
        .map(drop)
        .map_err(|e| {
            Stop::new(
                Reason::Unreachable,
                format!("cannot reach origin to record the line's head on main: {e}"),
                "check the network and your git credentials, then run the release again",
            )
        })
}

fn is_ancestor(git: &dyn GitRunner, a: &str, b: &str) -> Result<bool, Stop> {
    let asked = run(git, &["merge-base", "--is-ancestor", a, b])?;
    match asked.status {
        0 => Ok(true),
        1 => Ok(false),
        status => Err(unread(format!(
            "git merge-base --is-ancestor {a} {b} failed (exit {status}): {}",
            asked.stderr_utf8().trim()
        ))),
    }
}

fn tree_of(git: &dyn GitRunner, commit: &str) -> Result<String, Stop> {
    out(git, &["rev-parse", &format!("{commit}^{{tree}}")])
}

/// The paths `to` moves against `from`, renames as a deletion and an addition, submodule
/// pointers included whatever the config says.
fn moved(git: &dyn GitRunner, from: &str, to: &str) -> Result<Vec<String>, Stop> {
    let listed = ledger::git_ok(
        git,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            from,
            to,
        ],
    )
    .map_err(|e| unread(e.to_string()))?;
    Ok(listed
        .stdout
        .split(|b| *b == 0)
        .filter(|path| !path.is_empty())
        .map(lossy)
        .collect())
}

fn frozen(paths: &[String]) -> Vec<&str> {
    paths
        .iter()
        .map(String::as_str)
        .filter(|path| FROZEN.contains(path))
        .collect()
}

/// The release version `commit`'s manifest declares, or None when the commit has no manifest or
/// it declares none.
///
/// # Errors
/// COULD NOT RUN for a commit this clone lacks.
fn version_at(git: &dyn GitRunner, commit: &str) -> Result<Option<String>, Stop> {
    let shown = run(git, &["cat-file", "blob", &format!("{commit}:{MANIFEST}")])?;
    if shown.success() {
        return Ok(crate::publish::workspace_version(&shown.stdout_utf8()).ok());
    }
    if run(git, &["cat-file", "-e", &format!("{commit}^{{commit}}")])?.success() {
        Ok(None)
    } else {
        Err(lacks("commit", commit))
    }
}
