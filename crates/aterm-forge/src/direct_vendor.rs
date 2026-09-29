// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `vendor/astream`, the astream GIT SUBMODULE. This is a separate, named
//! `[OB-1]` provenance obligation, not an exemption for directories outside
//! the patch table.
//!
//! # The pin is the gitlink
//!
//! Until 2026-09-24 this directory was a hand-synced copy of four astream
//! crates, pinned file by file in a review record whose own digest, and the
//! upstream revision it named, were compiled in here. It is now a submodule of
//! `github.com/alabsystems/astream` (`vendor/README-astream.md`), and the one
//! pin is the GITLINK: the commit this repository's index records at
//! `vendor/astream`. Nothing below hardcodes an astream revision or a file
//! digest. A bump is a change to the gitlink, reviewed in astream as the range
//! it moves over. What this module proves is that the tree really builds the
//! commit the gitlink names:
//!
//! 1. `.gitmodules` declares path `vendor/astream` with astream's URL;
//! 2. the index records `vendor/astream` as ONE gitlink (mode 160000) and
//!    tracks nothing beneath it;
//! 3. the submodule is initialised and checked out at exactly the gitlink
//!    commit, with no tracked modification (untracked or ignored build output
//!    such as `target/` is not a modification);
//! 4. cargo resolves `astream-{aead,broker,cap,wire}` exactly once each, from
//!    `vendor/astream/crates/<name>/Cargo.toml`, as path packages (a null
//!    `source`) licensed Apache-2.0, and NOT as aterm workspace members;
//! 5. the root manifest's `[workspace] exclude` covers `vendor/astream`;
//! 6. `crates/aterm-link` still reaches astream-broker and astream-cap there;
//! 7. the pin did not move BACKWARD: relative to the merge-base with
//!    `origin/main`, a bump goes forward or nowhere (see [`PinMotion`]).
//!
//! The gitlink is read from the INDEX rather than from `HEAD`'s tree. The
//! index is what the next commit records, so a staged bump is judged as the
//! commit it is about to become; on a committed tree the two are the same.
//! Obligation 7 compares against `HEAD`'s merge-base with `origin/main`, so
//! mid-merge (`MERGE_HEAD` set, merge commit not yet made) it is blind to the
//! merge's own result and judges the merge commit once it exists.
//!
//! # What it does not prove
//!
//! It runs offline and never fetches, so it cannot say that the pinned commit
//! is one astream's `main` reached or that astream's own gate passed there.
//! Nor does it decide provenance: [`crate::provenance::FIRST_PARTY_VENDORED`]
//! is the reviewed claim that these crates are aterm's own code.

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_json::Value;
use aterm_toml::edit::{DocumentMut, Item};

/// The directory under `vendor/` this obligation owns.
pub(crate) const DIRECTORY: &str = "astream";
const SUBMODULE: &str = "vendor/astream";
/// Accepted with or without a trailing `.git`, and in no other spelling.
const URL: &str = "https://github.com/alabsystems/astream";
const GITLINK_MODE: &str = "160000";
/// The line of work the pin is judged against. The full ref name, so a local
/// branch or tag spelled `origin/main` cannot stand in for it.
const BASE_REF: &str = "refs/remotes/origin/main";
/// How messages spell [`BASE_REF`].
const BASE_NAME: &str = "origin/main";
/// A tip-commit message line that declares a DELIBERATE backward move, with
/// its reason: `astream-pin-rollback: astream 5021230 broke the sealed wire`.
const ROLLBACK_TRAILER: &str = "astream-pin-rollback:";
/// The astream crates aterm resolves. `astream-aead` only under aterm-link's
/// off-by-default `sealed` feature, which is why [`metadata`] asks for every
/// feature.
const PACKAGES: [&str; 4] = [
    "astream-aead",
    "astream-broker",
    "astream-cap",
    "astream-wire",
];
const CONSUMER: &str = "crates/aterm-link/Cargo.toml";
/// aterm-link's direct path dependencies into the submodule.
const CONSUMER_DEPS: [&str; 2] = ["astream-broker", "astream-cap"];
/// The variables that point git at a repository. A hook exports them for the
/// SUPERPROJECT, so they are cleared before asking git anything inside the
/// submodule, which is what git's own submodule code does too.
const REPO_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// What `[OB-1]` prints on success: the pin, read from this tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    /// The commit the index's gitlink records, which is also the checkout.
    pub(crate) commit: String,
    /// The URL `.gitmodules` declares for it.
    pub(crate) url: String,
}

/// One index entry at or beneath `vendor/astream`, as `git ls-files -s`
/// prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexEntry {
    mode: String,
    object: String,
    stage: String,
    path: String,
}

/// The submodule's own working tree, as git inside it reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Checkout {
    /// No repository of its own at `vendor/astream`, with the reason.
    Uninitialised(String),
    At {
        /// `HEAD` inside the submodule.
        head: String,
        /// `git status --porcelain -z --untracked-files=no` entries: staged or
        /// unstaged changes to tracked files. Untracked and ignored files are
        /// deliberately absent.
        modified: Vec<String>,
    },
}

/// Where this tree's pin stands against the pin `origin/main` had at the
/// merge-base. A bump moves FORWARD in astream's history; the way it goes
/// backward by accident is a checkout whose submodule the pull did not
/// advance, where `git commit -a` or `git add -A` (or a merge resolved from
/// that checkout) records the OLD gitlink. Seen 2026-09-29: main's pin went
/// back from astream `73e837b` to its ancestor `5021230` after being bumped.
///
/// A provable regression fails, and so does a pin that cannot be a bump
/// because the base's pin was never fetched ([`PinMotion::BaseNotFetched`]);
/// everything else passes. This module never fetches, so a pin commit missing
/// from the submodule's store is [`PinMotion::Undecided`] and passes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PinMotion {
    /// The gitlink is the base's own, or there is nothing to compare with: no
    /// `origin/main`, or no gitlink at `vendor/astream` at the merge-base.
    Unchanged,
    /// The base's pin is an ancestor of this one: a bump.
    Forward,
    /// Neither pin is an ancestor of the other.
    Diverged,
    /// This pin is a strict ancestor of the base's: a regression. `declared`
    /// is the tip commit's `astream-pin-rollback:` reason when it names one.
    Backward {
        base: String,
        declared: Option<String>,
    },
    /// The base's pin is not in the submodule's object store. This pin IS
    /// (the checkout is at it), and a descendant of the base would carry the
    /// base with it, so it is no bump from it: a stale checkout that never
    /// fetched what main moved on to, which is where the incident began.
    BaseNotFetched { base: String },
    /// Could not be decided, with the reason.
    Undecided(String),
}

/// Everything [`validate`] judges. [`capture`] reads it from the real
/// repository and the tests mutate it.
#[derive(Clone, Debug)]
struct Facts {
    /// `.gitmodules` as `(key, value)` pairs, as `git config --list` reads
    /// the file; empty when there is no `.gitmodules`.
    gitmodules: Vec<(String, String)>,
    index: Vec<IndexEntry>,
    checkout: Checkout,
    /// The pin against `origin/main`'s, at the merge-base.
    motion: PinMotion,
    /// The root manifest's `[workspace] exclude` entries.
    exclude: Vec<String>,
    /// Held as a result so that the git facts, which explain WHY cargo
    /// failed (an uninitialised submodule), are judged first.
    metadata: Result<Value, String>,
}

/// Called only for the named submodule or its live consumer. Other vendor
/// directories still owe the ordinary patch-table obligation. `Ok(None)`
/// means neither exists at this root (attest's synthetic fixtures).
pub(crate) fn review(root: &Path) -> Result<Option<Pin>, String> {
    if !root.join(SUBMODULE).exists() && !root.join(CONSUMER).exists() {
        return Ok(None);
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    let facts = capture(&root)?;
    validate(&root, &facts).map(Some)
}

fn capture(root: &Path) -> Result<Facts, String> {
    let index = index(root)?;
    Ok(Facts {
        gitmodules: gitmodules(root)?,
        index: index.clone(),
        checkout: checkout(root),
        motion: pin_motion(root, &index),
        exclude: exclude(root)?,
        metadata: metadata(root),
    })
}

fn git(dir: &Path, args: &[&str], isolate: bool) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    if isolate {
        for var in REPO_ENV {
            cmd.env_remove(var);
        }
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run `git {}`: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` in {} refused: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn nul_separated(bytes: Vec<u8>) -> Result<Vec<String>, String> {
    Ok(String::from_utf8(bytes)
        .map_err(|e| format!("non-UTF-8 git output: {e}"))?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Read by git, not by hand: `.gitmodules` is git-config syntax (quoting,
/// comments, case rules), and git's reading of it is the one that fetches.
fn gitmodules(root: &Path) -> Result<Vec<(String, String)>, String> {
    if !root.join(".gitmodules").is_file() {
        return Ok(Vec::new());
    }
    let out = git(
        root,
        &["config", "--file", ".gitmodules", "--null", "--list"],
        false,
    )?;
    Ok(nul_separated(out)?
        .into_iter()
        .map(|entry| match entry.split_once('\n') {
            Some((k, v)) => (k.to_owned(), v.to_owned()),
            None => (entry, String::new()),
        })
        .collect())
}

fn index(root: &Path) -> Result<Vec<IndexEntry>, String> {
    let out = git(root, &["ls-files", "-s", "-z", "--", SUBMODULE], false)?;
    nul_separated(out)?
        .into_iter()
        .map(|line| {
            let (meta, path) = line
                .split_once('\t')
                .ok_or_else(|| format!("unreadable index entry: {line}"))?;
            let mut f = meta.split(' ');
            match (f.next(), f.next(), f.next(), f.next()) {
                (Some(mode), Some(object), Some(stage), None) => Ok(IndexEntry {
                    mode: mode.to_owned(),
                    object: object.to_owned(),
                    stage: stage.to_owned(),
                    path: path.to_owned(),
                }),
                _ => Err(format!("unreadable index entry: {line}")),
            }
        })
        .collect()
}

/// Never an error: whatever keeps git from answering INSIDE the submodule is
/// the finding, and [`validate`] names the fix for it.
fn checkout(root: &Path) -> Checkout {
    let dir = root.join(SUBMODULE);
    if !dir.is_dir() {
        return Checkout::Uninitialised("the directory does not exist".into());
    }
    // An uninitialised submodule is an empty directory, and git run inside it
    // silently answers for the SUPERPROJECT; its HEAD would then be read as
    // the submodule's. The top level must be this directory itself.
    let top = match git(&dir, &["rev-parse", "--show-toplevel"], true) {
        Ok(out) => String::from_utf8_lossy(&out).trim().to_owned(),
        Err(e) => return Checkout::Uninitialised(e),
    };
    if Path::new(&top) != dir {
        return Checkout::Uninitialised(format!(
            "git inside it answers for the repository at {top}"
        ));
    }
    let head = match git(
        &dir,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        true,
    ) {
        Ok(out) => String::from_utf8_lossy(&out).trim().to_owned(),
        Err(e) => return Checkout::Uninitialised(format!("it has no HEAD commit: {e}")),
    };
    // `--no-optional-locks`: a status must not rewrite the submodule's index.
    let status = git(
        &dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
        ],
        true,
    );
    match status.and_then(nul_separated) {
        Ok(modified) => Checkout::At { head, modified },
        Err(e) => Checkout::Uninitialised(format!("its status is unreadable: {e}")),
    }
}

/// Never an error: what git cannot answer is [`PinMotion::Undecided`], and an
/// unreadable gitlink is [`validate`]'s finding, not this function's.
fn pin_motion(root: &Path, index: &[IndexEntry]) -> PinMotion {
    let Ok(pin) = gitlink(index) else {
        return PinMotion::Unchanged;
    };
    let base_tip = format!("{BASE_REF}^{{commit}}");
    if git(
        root,
        &["rev-parse", "--verify", "--quiet", &base_tip],
        false,
    )
    .is_err()
    {
        return PinMotion::Unchanged;
    }
    let merge_base = match git(root, &["merge-base", "HEAD", BASE_REF], false) {
        Ok(out) => String::from_utf8_lossy(&out).trim().to_owned(),
        Err(e) => return PinMotion::Undecided(format!("no merge-base with {BASE_REF}: {e}")),
    };
    // The gitlink at the merge-base. No entry there, or one that is not a
    // gitlink, means there is no earlier pin to fall behind.
    let base = match git(
        root,
        &["ls-tree", "-z", &merge_base, "--", SUBMODULE],
        false,
    )
    .and_then(nul_separated)
    {
        Ok(entries) => match entries.iter().find_map(|e| {
            let (meta, path) = e.split_once('\t')?;
            let mut f = meta.split(' ');
            let (mode, _kind, object) = (f.next()?, f.next()?, f.next()?);
            (path == SUBMODULE && mode == GITLINK_MODE).then(|| object.to_owned())
        }) {
            Some(object) => object,
            None => return PinMotion::Unchanged,
        },
        Err(e) => return PinMotion::Undecided(format!("cannot read {merge_base}: {e}")),
    };
    if base == pin {
        return PinMotion::Unchanged;
    }
    let sub = root.join(SUBMODULE);
    let held = |commit: &str| {
        git(
            &sub,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
            true,
        )
        .is_ok()
    };
    if !held(&pin) {
        return PinMotion::Undecided(format!(
            "astream commit {pin} is not in {SUBMODULE}'s object store"
        ));
    }
    if !held(&base) {
        return PinMotion::BaseNotFetched { base };
    }
    match (
        is_ancestor(&sub, &base, &pin),
        is_ancestor(&sub, &pin, &base),
    ) {
        (Ok(true), _) => PinMotion::Forward,
        (Ok(false), Ok(true)) => PinMotion::Backward {
            declared: rollback_reason(root, &merge_base),
            base,
        },
        (Ok(false), Ok(false)) => PinMotion::Diverged,
        (Err(e), _) | (_, Err(e)) => PinMotion::Undecided(e),
    }
}

/// `git merge-base --is-ancestor`: exit 0 yes, 1 no, anything else an error,
/// which [`git`] cannot tell apart from "no".
fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let mut cmd = Command::new("git");
    cmd.args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(dir);
    for var in REPO_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run `git merge-base --is-ancestor`: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "`git merge-base --is-ancestor {ancestor} {descendant}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// The reason a commit on this branch (`merge_base..HEAD`) gives for moving
/// the pin backward, if any does. A rollback usually needs aterm changes for
/// the older astream on top of it, so the trailer is not confined to the tip.
/// It must start its line: an indented or quoted mention is documentation.
fn rollback_reason(root: &Path, merge_base: &str) -> Option<String> {
    let range = format!("{merge_base}..HEAD");
    let log = git(root, &["log", "--format=%B%x00", &range], false).ok()?;
    String::from_utf8_lossy(&log).lines().find_map(|line| {
        let reason = line.strip_prefix(ROLLBACK_TRAILER)?.trim();
        (!reason.is_empty()).then(|| reason.to_owned())
    })
}

fn exclude(root: &Path) -> Result<Vec<String>, String> {
    let path = root.join("Cargo.toml");
    let doc: DocumentMut = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(doc
        .get("workspace")
        .and_then(|w| w.get("exclude"))
        .and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

/// The FULL resolution, not `--no-deps`: that lists workspace members only,
/// and the astream crates are excluded from this workspace by design, so it
/// could never see them. `--all-features` because `astream-aead` is reachable
/// only through aterm-link's off-by-default `sealed` feature. Still `--locked
/// --offline`: this reads the lock that ships and fetches nothing.
fn metadata(root: &Path) -> Result<Value, String> {
    // `$CARGO` (the driver running forge), else Trust's `targo` — never a stock
    // `cargo`. `metadata` takes no lane flag.
    let exe = std::env::var_os("CARGO").unwrap_or_else(|| "targo".into());
    let out = Command::new(exe)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--locked",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot read Cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Cargo metadata refused: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    aterm_json::from_slice(&out.stdout).map_err(|e| format!("invalid Cargo metadata: {e}"))
}

fn validate(root: &Path, facts: &Facts) -> Result<Pin, String> {
    let url = declared_url(&facts.gitmodules)?;
    let commit = gitlink(&facts.index)?;
    checked_out_at(&facts.checkout, &commit)?;
    pin_did_not_move_back(&facts.motion, &commit)?;
    excluded(&facts.exclude)?;
    let metadata = facts.metadata.as_ref().map_err(Clone::clone)?;
    resolved_from_submodule(root, metadata)?;
    consumer_reaches_submodule(root, metadata)?;
    Ok(Pin { commit, url })
}

/// (7) The pin did not go backward against `origin/main` at the merge-base.
fn pin_did_not_move_back(motion: &PinMotion, commit: &str) -> Result<(), String> {
    match motion {
        PinMotion::Backward {
            base,
            declared: None,
        } => Err(format!(
            "this tree pins `{SUBMODULE}` at {commit}, an ANCESTOR of {base}, the pin \
             {BASE_NAME} had at the merge-base — a bump moves forward only. A checkout whose \
             submodule a pull never advanced leaves this behind (`git commit -a` / `git add -A` \
             then records the OLD gitlink). To repair it: `git -C {SUBMODULE} checkout {base} && \
             git add {SUBMODULE}`, then commit or amend — `git submodule update --init` cannot \
             help once the regressed gitlink is what is staged. To prevent it: `git submodule \
             update --init {SUBMODULE}` BEFORE committing, and `git config submodule.recurse \
             true`. A DELIBERATE rollback says so in a commit message on this branch, on a line \
             that starts `{ROLLBACK_TRAILER} <reason>`"
        )),
        PinMotion::BaseNotFetched { base } => Err(format!(
            "{BASE_NAME} pinned `{SUBMODULE}` at {base} at the merge-base, and this checkout's \
             submodule has never fetched it, so {commit} cannot be a bump from it (a descendant \
             would hold it) — the shape of a stale checkout re-recording an old gitlink. Run \
             `git -C {SUBMODULE} fetch origin`, then re-run: a real bump passes, and a rollback \
             needs a line starting `{ROLLBACK_TRAILER} <reason>` in a commit message on this branch"
        )),
        _ => Ok(()),
    }
}

/// A short, bounded rendering of a path list for a one-line failure.
fn preview(items: &[&str]) -> String {
    const SHOWN: usize = 5;
    let mut s = items
        .iter()
        .take(SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > SHOWN {
        s.push_str(&format!(", … {} more", items.len() - SHOWN));
    }
    s
}

/// (1) `.gitmodules` names this path exactly once, with astream's URL.
fn declared_url(entries: &[(String, String)]) -> Result<String, String> {
    let names: Vec<&str> = entries
        .iter()
        .filter_map(|(key, value)| {
            let name = key.strip_prefix("submodule.")?.strip_suffix(".path")?;
            (value.trim_end_matches('/') == SUBMODULE).then_some(name)
        })
        .collect();
    let name = match names.as_slice() {
        [one] => *one,
        [] => {
            return Err(format!(
                "`.gitmodules` declares no submodule at path `{SUBMODULE}` — restore its \
                 `[submodule \"{SUBMODULE}\"]` entry (path = {SUBMODULE}, url = {URL}.git); \
                 without it `git submodule update --init` has nothing to fetch the pin from"
            ));
        }
        many => {
            return Err(format!(
                "`.gitmodules` declares path `{SUBMODULE}` {} times ({}) — keep exactly one entry",
                many.len(),
                many.join(", ")
            ));
        }
    };
    let key = format!("submodule.{name}.url");
    // git reads the LAST value of a single-valued key.
    let url = entries
        .iter()
        .rev()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v);
    match url {
        Some(u) if u == URL || u.strip_suffix(".git") == Some(URL) => Ok(u.clone()),
        Some(u) => Err(format!(
            "`.gitmodules` points `{SUBMODULE}` at `{u}`, which is not astream ({URL}, with or \
             without `.git`) — restore the url and run `git submodule sync {SUBMODULE}`"
        )),
        None => Err(format!(
            "`.gitmodules` entry `{name}` declares path `{SUBMODULE}` with no url — set \
             url = {URL}.git"
        )),
    }
}

/// (2) The index holds one gitlink at the path and nothing beneath it.
/// Returns the pinned commit.
fn gitlink(index: &[IndexEntry]) -> Result<String, String> {
    let beneath: Vec<&str> = index
        .iter()
        .filter(|e| e.path != SUBMODULE)
        .map(|e| e.path.as_str())
        .collect();
    if !beneath.is_empty() {
        return Err(format!(
            "the index tracks {} path(s) beneath `{SUBMODULE}` ({}) — that content belongs to \
             astream's repository, not this one: `git rm -r --cached` them so the gitlink is \
             the only entry (vendor/README-astream.md)",
            beneath.len(),
            preview(&beneath)
        ));
    }
    let entry = match index {
        [one] => one,
        [] => {
            return Err(format!(
                "the index records nothing at `{SUBMODULE}`, so no astream commit is pinned — \
                 add the submodule back (`git submodule add {URL}.git {SUBMODULE}`) and commit \
                 the gitlink"
            ));
        }
        many => {
            return Err(format!(
                "the index records `{SUBMODULE}` {} times (an unmerged submodule) — choose the \
                 commit, check it out in the submodule and `git add {SUBMODULE}`",
                many.len()
            ));
        }
    };
    if entry.mode != GITLINK_MODE {
        return Err(format!(
            "the index records `{SUBMODULE}` with mode {}, not as a gitlink ({GITLINK_MODE}) — the \
             bus arrives as the astream submodule, never as files committed here \
             (vendor/README-astream.md)",
            entry.mode
        ));
    }
    if entry.stage != "0" {
        return Err(format!(
            "the gitlink at `{SUBMODULE}` is unmerged (stage {}) — choose the commit, check it \
             out in the submodule and `git add {SUBMODULE}`",
            entry.stage
        ));
    }
    let is_object_id = matches!(entry.object.len(), 40 | 64)
        && entry.object.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_object_id {
        return Err(format!(
            "the gitlink at `{SUBMODULE}` names `{}`, which is not a commit id",
            entry.object
        ));
    }
    Ok(entry.object.clone())
}

/// (3) The checkout is the pinned commit, unmodified.
fn checked_out_at(checkout: &Checkout, commit: &str) -> Result<(), String> {
    match checkout {
        Checkout::Uninitialised(why) => Err(format!(
            "`{SUBMODULE}` is not an initialised submodule checkout ({why}) — run \
             `git submodule update --init {SUBMODULE}`"
        )),
        Checkout::At { head, .. } if head != commit => Err(format!(
            "`{SUBMODULE}` is checked out at {head}, but this tree's gitlink pins {commit} — \
             `git submodule update --init {SUBMODULE}` returns it to the pin; a deliberate bump \
             is committed instead (`git add {SUBMODULE} Cargo.lock`, then commit the bump)"
        )),
        Checkout::At { modified, .. } if !modified.is_empty() => {
            let shown: Vec<&str> = modified.iter().map(String::as_str).collect();
            Err(format!(
                "`{SUBMODULE}` has {} tracked modification(s) at the pinned commit ({}) — the \
                 bus is edited in astream and arrives here as a bump: land the change there and \
                 commit the bump, or discard it (`git -C {SUBMODULE} restore --staged \
                 --worktree .`)",
                modified.len(),
                preview(&shown)
            ))
        }
        Checkout::At { .. } => Ok(()),
    }
}

/// (5) cargo leaves the submodule to astream's workspace. Cargo's rule is a
/// path prefix, so an entry naming an ancestor directory covers it too.
fn excluded(exclude: &[String]) -> Result<(), String> {
    let covers = |entry: &String| {
        let entry = entry.trim_start_matches("./").trim_end_matches('/');
        !entry.is_empty() && Path::new(SUBMODULE).starts_with(entry)
    };
    if exclude.iter().any(covers) {
        return Ok(());
    }
    Err(format!(
        "the root Cargo.toml's `[workspace] exclude` does not cover `{SUBMODULE}` — without it \
         cargo makes every astream crate an aterm member and resolves its `workspace = true` \
         against aterm's manifest; add `exclude = [\"{SUBMODULE}\"]`"
    ))
}

fn packages_and_members(metadata: &Value) -> Result<(&Vec<Value>, &Vec<Value>), String> {
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("Cargo metadata has no packages")?;
    let members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or("Cargo metadata has no workspace members")?;
    Ok((packages, members))
}

/// (4) Each astream crate resolves once, from the submodule, as a path
/// package outside this workspace.
fn resolved_from_submodule(root: &Path, metadata: &Value) -> Result<(), String> {
    let (packages, members) = packages_and_members(metadata)?;
    for name in PACKAGES {
        let found: Vec<&Value> = packages
            .iter()
            .filter(|p| p.get("name").and_then(Value::as_str) == Some(name))
            .collect();
        let [p] = found.as_slice() else {
            return Err(format!(
                "Cargo metadata resolves {} `{name}` package(s); the gitlink pins exactly one, \
                 from `{SUBMODULE}/crates/{name}`",
                found.len()
            ));
        };
        let want = root
            .join(SUBMODULE)
            .join("crates")
            .join(name)
            .join("Cargo.toml");
        let actual = p.get("manifest_path").and_then(Value::as_str);
        if actual.map(PathBuf::from).as_ref() != Some(&want) {
            return Err(format!(
                "Cargo metadata redirects `{name}` to {}, away from the pinned submodule's \
                 `{SUBMODULE}/crates/{name}`",
                actual.unwrap_or("nowhere")
            ));
        }
        if !p.get("source").is_some_and(Value::is_null) {
            return Err(format!(
                "Cargo metadata resolves `{name}` from source {}, not as a path package in the \
                 submodule",
                p.get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("(unrecorded)")
            ));
        }
        if p.get("license").and_then(Value::as_str) != Some("Apache-2.0") {
            return Err(format!(
                "`{name}` in the submodule declares license {}, not Apache-2.0",
                p.get("license").and_then(Value::as_str).unwrap_or("(none)")
            ));
        }
        if members.iter().any(|id| Some(id) == p.get("id")) {
            return Err(format!(
                "`{name}` is an aterm workspace member — it belongs to astream's workspace and \
                 inherits astream's package fields; keep `{SUBMODULE}` in the root manifest's \
                 `[workspace] exclude`"
            ));
        }
    }
    Ok(())
}

/// (6) aterm-link, the member that is the reason the submodule exists, still
/// path-depends on the submodule's crates.
fn consumer_reaches_submodule(root: &Path, metadata: &Value) -> Result<(), String> {
    let (packages, _) = packages_and_members(metadata)?;
    let consumer = packages
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some("aterm-link"))
        .ok_or("the astream submodule has no aterm-link consumer")?;
    if consumer
        .get("manifest_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        != Some(root.join(CONSUMER))
    {
        return Err(format!(
            "aterm-link is not the workspace member at {CONSUMER}"
        ));
    }
    let deps = consumer
        .get("dependencies")
        .and_then(Value::as_array)
        .ok_or("aterm-link has no dependencies")?;
    for name in CONSUMER_DEPS {
        let dep = deps
            .iter()
            .find(|d| d.get("name").and_then(Value::as_str) == Some(name))
            .ok_or_else(|| format!("aterm-link no longer uses {name}"))?;
        if dep.get("path").and_then(Value::as_str).map(PathBuf::from)
            != Some(root.join(SUBMODULE).join("crates").join(name))
        {
            return Err(format!(
                "aterm-link redirects {name} away from `{SUBMODULE}/crates/{name}`"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .canonicalize()
            .unwrap()
    }

    /// The real tree's facts, which every mutation below starts from, and
    /// which must themselves validate or the mutations prove nothing.
    fn real() -> (PathBuf, Facts) {
        let root = repo_root();
        let facts = capture(&root).unwrap();
        assert!(
            validate(&root, &facts).is_ok(),
            "{:?}",
            validate(&root, &facts)
        );
        (root, facts)
    }

    fn rejects(root: &Path, facts: &Facts, needles: &[&str]) {
        let why = validate(root, facts).expect_err("the mutation must be refused");
        for needle in needles {
            assert!(why.contains(needle), "expected `{needle}` in: {why}");
        }
    }

    fn package<'a>(facts: &'a mut Facts, name: &str) -> &'a mut aterm_json::Map {
        facts
            .metadata
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("packages")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|p| p["name"].as_str() == Some(name))
            .unwrap()
            .as_object_mut()
            .unwrap()
    }

    #[test]
    fn the_real_submodule_is_the_pinned_commit_and_cargo_resolves_it() {
        let root = repo_root();
        let pin = review(&root)
            .unwrap_or_else(|why| panic!("{why}"))
            .expect("this checkout has vendor/astream");
        let (_, facts) = real();
        assert_eq!(pin.commit, facts.index[0].object);
        assert!(pin.url == URL || pin.url == format!("{URL}.git"));
    }

    #[test]
    fn a_missing_or_wrong_gitmodules_entry_is_refused() {
        let (root, facts) = real();
        let mut missing = facts.clone();
        missing.gitmodules.retain(|(k, _)| !k.ends_with(".path"));
        rejects(&root, &missing, &[".gitmodules", "declares no submodule"]);
        let mut absent = facts.clone();
        absent.gitmodules.clear();
        rejects(&root, &absent, &["declares no submodule"]);
        let mut wrong = facts.clone();
        for (k, v) in &mut wrong.gitmodules {
            if k.ends_with(".url") {
                *v = "https://github.com/someone-else/astream.git".into();
            }
        }
        rejects(&root, &wrong, &["someone-else", "git submodule sync"]);
        let mut twice = facts.clone();
        twice
            .gitmodules
            .push(("submodule.other.path".into(), SUBMODULE.into()));
        rejects(&root, &twice, &["2 times"]);
        // Both spellings of the one URL are accepted.
        for spelling in [URL.to_string(), format!("{URL}.git")] {
            let mut ok = facts.clone();
            for (k, v) in &mut ok.gitmodules {
                if k.ends_with(".url") {
                    v.clone_from(&spelling);
                }
            }
            assert_eq!(validate(&root, &ok).unwrap().url, spelling);
        }
    }

    #[test]
    fn a_non_gitlink_or_a_tracked_file_beneath_it_is_refused() {
        let (root, facts) = real();
        let mut tree = facts.clone();
        tree.index[0].mode = "100644".into();
        rejects(&root, &tree, &["mode 100644", "not as a gitlink"]);
        let mut extra = facts.clone();
        extra.index.push(IndexEntry {
            mode: "100644".into(),
            object: "0".repeat(40),
            stage: "0".into(),
            path: format!("{SUBMODULE}/crates/astream-cap/src/lib.rs"),
        });
        rejects(
            &root,
            &extra,
            &["tracks 1 path(s) beneath", "git rm -r --cached"],
        );
        let mut none = facts.clone();
        none.index.clear();
        rejects(&root, &none, &["records nothing"]);
    }

    #[test]
    fn a_checkout_off_the_gitlink_or_uninitialised_or_dirty_is_refused() {
        let (root, facts) = real();
        let mut moved = facts.clone();
        moved.checkout = Checkout::At {
            head: "0".repeat(40),
            modified: Vec::new(),
        };
        rejects(
            &root,
            &moved,
            &[
                "git submodule update --init vendor/astream",
                "commit the bump",
            ],
        );
        let mut bare = facts.clone();
        bare.checkout = Checkout::Uninitialised("empty".into());
        rejects(
            &root,
            &bare,
            &[
                "not an initialised",
                "git submodule update --init vendor/astream",
            ],
        );
        let mut dirty = facts.clone();
        let Checkout::At { modified, .. } = &mut dirty.checkout else {
            panic!("the real checkout is initialised")
        };
        modified.push(" M crates/astream-cap/src/lib.rs".into());
        rejects(
            &root,
            &dirty,
            &["1 tracked modification", "astream-cap/src/lib.rs"],
        );
    }

    #[test]
    fn a_package_redirected_made_a_member_or_given_a_registry_source_is_refused() {
        let (root, facts) = real();
        let mut redirected = facts.clone();
        package(&mut redirected, "astream-cap").insert(
            "manifest_path".into(),
            Value::String("/outside/Cargo.toml".into()),
        );
        rejects(&root, &redirected, &["redirects `astream-cap`", "/outside"]);
        let mut member = facts.clone();
        let id = package(&mut member, "astream-wire")["id"].clone();
        member
            .metadata
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("workspace_members")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(id);
        rejects(
            &root,
            &member,
            &["`astream-wire` is an aterm workspace member"],
        );
        let mut registry = facts.clone();
        package(&mut registry, "astream-broker").insert(
            "source".into(),
            Value::String("registry+https://github.com/rust-lang/crates.io-index".into()),
        );
        rejects(
            &root,
            &registry,
            &["`astream-broker` from source registry+"],
        );
        let mut licensed = facts.clone();
        package(&mut licensed, "astream-aead")
            .insert("license".into(), Value::String("MIT".into()));
        rejects(&root, &licensed, &["declares license MIT"]);
        let mut doubled = facts.clone();
        let copy = Value::Object(package(&mut doubled, "astream-cap").clone());
        doubled
            .metadata
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("packages")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(copy);
        rejects(&root, &doubled, &["resolves 2 `astream-cap` package(s)"]);
        let mut refused = facts;
        refused.metadata = Err("Cargo metadata refused: planted".into());
        rejects(&root, &refused, &["planted"]);
    }

    #[test]
    fn a_missing_exclude_is_refused_and_an_ancestor_exclude_covers_it() {
        let (root, facts) = real();
        let mut none = facts.clone();
        none.exclude.clear();
        rejects(&root, &none, &["exclude", "vendor/astream"]);
        let mut sibling = facts.clone();
        sibling.exclude = vec!["vendor/astream-other".into(), "vendor/ast".into()];
        rejects(&root, &sibling, &["exclude"]);
        let mut ancestor = facts;
        ancestor.exclude = vec!["vendor/".into()];
        assert!(validate(&root, &ancestor).is_ok());
    }

    #[test]
    fn aterm_link_redirected_away_from_the_submodule_is_refused() {
        let (root, mut facts) = real();
        let deps = package(&mut facts, "aterm-link")
            .get_mut("dependencies")
            .unwrap()
            .as_array_mut()
            .unwrap();
        for d in deps {
            if d["name"].as_str() == Some("astream-cap") {
                d.as_object_mut()
                    .unwrap()
                    .insert("path".into(), Value::String("/outside".into()));
            }
        }
        rejects(&root, &facts, &["aterm-link redirects astream-cap"]);
    }

    #[test]
    fn a_backward_pin_is_refused_unless_a_commit_on_the_branch_declares_a_rollback() {
        let (root, mut facts) = real();
        let base = "0123456789012345678901234567890123456789".to_owned();
        facts.motion = PinMotion::Backward {
            base: base.clone(),
            declared: None,
        };
        rejects(
            &root,
            &facts,
            &[
                "ANCESTOR",
                &base,
                "forward only",
                "submodule.recurse",
                ROLLBACK_TRAILER,
            ],
        );
        facts.motion = PinMotion::Backward {
            base,
            declared: Some("astream 5021230 broke the sealed wire".into()),
        };
        assert!(
            validate(&root, &facts).is_ok(),
            "a declared rollback passes"
        );
        // A base the checkout never fetched cannot be bumped from either.
        facts.motion = PinMotion::BaseNotFetched {
            base: "0123456789012345678901234567890123456789".into(),
        };
        rejects(
            &root,
            &facts,
            &["never fetched", "fetch origin", ROLLBACK_TRAILER],
        );
        // Everything else passes.
        for other in [
            PinMotion::Unchanged,
            PinMotion::Forward,
            PinMotion::Diverged,
            PinMotion::Undecided("no origin/main".into()),
        ] {
            facts.motion = other.clone();
            assert!(validate(&root, &facts).is_ok(), "{other:?} must pass");
        }
    }

    /// A throwaway superproject whose `vendor/astream` is a real repository,
    /// so `pin_motion` runs the git it runs in production.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let root =
                std::env::temp_dir().join(format!("aterm-forge-pin-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join(SUBMODULE)).unwrap();
            let f = Fixture { root };
            f.run(&f.root, &["init", "-q", "-b", "main"]);
            f.run(&f.root.join(SUBMODULE), &["init", "-q", "-b", "main"]);
            f
        }

        fn run(&self, dir: &Path, args: &[&str]) -> String {
            let mut cmd = Command::new("git");
            cmd.args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir);
            // Whatever git repository the caller exports would otherwise take
            // these commits — and this fixture writes `origin/main`.
            for var in REPO_ENV {
                cmd.env_remove(var);
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        /// A commit in the SUBMODULE on top of `parent` (or the current HEAD).
        fn astream_commit(&self, parent: Option<&str>, msg: &str) -> String {
            let sub = self.root.join(SUBMODULE);
            if let Some(p) = parent {
                self.run(&sub, &["checkout", "-q", "--detach", p]);
            }
            self.run(&sub, &["commit", "-q", "--allow-empty", "-m", msg]);
            self.run(&sub, &["rev-parse", "HEAD"])
        }

        /// A commit in the SUPERPROJECT that records `pin` at the gitlink.
        fn commit_pin(&self, parent: Option<&str>, pin: &str, msg: &str) -> String {
            if let Some(p) = parent {
                self.run(&self.root, &["checkout", "-q", "--detach", p]);
            }
            let cacheinfo = format!("160000,{pin},{SUBMODULE}");
            self.run(
                &self.root,
                &["update-index", "--add", "--cacheinfo", &cacheinfo],
            );
            self.run(&self.root, &["commit", "-q", "-m", msg]);
            self.run(&self.root, &["rev-parse", "HEAD"])
        }

        fn motion(&self) -> PinMotion {
            // `pin_motion` asks the superproject with the caller's environment,
            // as production does (a hook's GIT_INDEX_FILE is honoured there),
            // so an exported repository variable would aim it at that repository.
            for var in REPO_ENV {
                assert!(
                    std::env::var_os(var).is_none(),
                    "{var} is exported: unset it, or this test reads a real repository"
                );
            }
            pin_motion(&self.root, &index(&self.root).unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn the_pin_is_judged_against_origin_main_at_the_merge_base_with_real_git() {
        let f = Fixture::new("motion");
        let c1 = f.astream_commit(None, "c1");
        let c2 = f.astream_commit(None, "c2");
        let c3 = f.astream_commit(None, "c3");
        let sibling = f.astream_commit(Some(&c1), "sibling of c2");

        // No origin/main at all: nothing to fall behind.
        let base = f.commit_pin(None, &c2, "base: the pin is c2");
        assert_eq!(
            f.motion(),
            PinMotion::Unchanged,
            "no {BASE_REF} to judge by"
        );
        f.run(&f.root, &["update-ref", "refs/remotes/origin/main", &base]);

        // The gitlink is the base's own.
        assert_eq!(f.motion(), PinMotion::Unchanged);

        // The bump goes forward, and stays forward through a later commit.
        f.commit_pin(Some(&base), &c3, "bump to c3");
        assert_eq!(f.motion(), PinMotion::Forward);

        // THE INCIDENT: a stale checkout commits the old gitlink back.
        f.commit_pin(Some(&base), &c1, "an unrelated fix that drags the pin back");
        assert_eq!(
            f.motion(),
            PinMotion::Backward {
                base: c2.clone(),
                declared: None
            }
        );

        // The same regression, declared, is a rollback and says why.
        f.commit_pin(
            Some(&base),
            &c1,
            "revert astream\n\nastream-pin-rollback: c2 broke the sealed wire",
        );
        let declared = PinMotion::Backward {
            base: c2.clone(),
            declared: Some("c2 broke the sealed wire".into()),
        };
        assert_eq!(f.motion(), declared);
        // …and it still counts once more commits sit on top of it, which is how
        // a rollback lands (aterm changes for the older astream follow it).
        f.run(
            &f.root,
            &["commit", "-q", "--allow-empty", "-m", "adapt aterm-link"],
        );
        assert_eq!(
            f.motion(),
            declared,
            "the trailer is not confined to the tip"
        );
        // An empty reason, an indented mention and a quoted one declare nothing.
        for msg in [
            "revert\n\nastream-pin-rollback:   ",
            "revert\n\n    astream-pin-rollback: <reason> is the escape hatch",
            "revert\n\n> astream-pin-rollback: quoted",
        ] {
            f.commit_pin(Some(&base), &c1, msg);
            assert!(
                matches!(f.motion(), PinMotion::Backward { declared: None, .. }),
                "{msg:?} must not declare a rollback"
            );
        }

        // Neither pin descends from the other.
        f.commit_pin(Some(&base), &sibling, "sideways to a sibling of c2");
        assert_eq!(f.motion(), PinMotion::Diverged);

        // A pin whose commit this checkout does not hold cannot be decided
        // offline, and an undecided pin never fails the gate.
        f.commit_pin(Some(&base), &"f".repeat(40), "a pin nobody fetched");
        assert!(
            matches!(f.motion(), PinMotion::Undecided(_)),
            "{:?}",
            f.motion()
        );
    }

    #[test]
    fn a_stale_checkout_that_never_fetched_the_base_is_caught_not_skipped() {
        let f = Fixture::new("stale");
        let c1 = f.astream_commit(None, "c1");
        // main moved on to a commit this checkout's submodule never fetched.
        let unfetched = "a".repeat(40);
        let base = f.commit_pin(None, &unfetched, "main: pinned to a commit not held here");
        f.run(&f.root, &["update-ref", "refs/remotes/origin/main", &base]);
        f.commit_pin(
            Some(&base),
            &c1,
            "an unrelated fix re-recording the old gitlink",
        );
        assert_eq!(
            f.motion(),
            PinMotion::BaseNotFetched { base: unfetched },
            "a pin the checkout holds, against a base it does not, is no bump from it"
        );
    }

    #[test]
    fn no_gitlink_at_the_merge_base_means_no_earlier_pin_to_fall_behind() {
        let f = Fixture::new("nobase");
        let c1 = f.astream_commit(None, "c1");
        // The base commit tracks something else and has no vendor/astream at all.
        std::fs::write(f.root.join("README"), "x").unwrap();
        f.run(&f.root, &["add", "README"]);
        f.run(&f.root, &["commit", "-q", "-m", "base: no submodule yet"]);
        let base = f.run(&f.root, &["rev-parse", "HEAD"]);
        f.run(&f.root, &["update-ref", "refs/remotes/origin/main", &base]);
        f.commit_pin(Some(&base), &c1, "add the submodule");
        assert_eq!(f.motion(), PinMotion::Unchanged);
    }

    #[test]
    fn a_local_branch_named_origin_main_is_not_the_base() {
        let f = Fixture::new("shadow");
        let c1 = f.astream_commit(None, "c1");
        let c2 = f.astream_commit(None, "c2");
        let base = f.commit_pin(None, &c2, "the pin is c2");
        // The remote-tracking ref is absent, and a LOCAL branch spells its name.
        f.run(&f.root, &["branch", "origin/main", &base]);
        f.commit_pin(Some(&base), &c1, "goes back to c1");
        assert_eq!(f.motion(), PinMotion::Unchanged);
    }
}
