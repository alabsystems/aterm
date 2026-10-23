// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `vendor/astream` git submodule provenance check (`[OB-1]`).

use aterm_uds::output::CommandOutput as _;
use aterm_uds::spawnfd::command;
use std::path::{Path, PathBuf};

use aterm_json::Value;
use aterm_toml::edit::{DocumentMut, Item};

/// The directory under `vendor/` this obligation owns.
pub(crate) const DIRECTORY: &str = "astream";
const SUBMODULE: &str = "vendor/astream";
const URL: &str = "https://github.com/alabsystems/astream";
const GITLINK_MODE: &str = "160000";
const BASE_REF: &str = "refs/remotes/origin/main";
const BASE_NAME: &str = "origin/main";
const ROLLBACK_TRAILER: &str = "astream-pin-rollback:";
const CRATE_PREFIX: &str = "astream-";
const CONSUMER: &str = "crates/aterm-link/Cargo.toml";
const CONSUMER_DEPS: [&str; 2] = ["astream-broker", "astream-cap"];
const REPO_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Validated submodule pin read from this tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    /// The commit recorded by the index gitlink and checked out.
    pub(crate) commit: String,
    /// The URL `.gitmodules` declares for it.
    pub(crate) url: String,
    /// Sorted astream crates Cargo resolves from the submodule.
    pub(crate) crates: Vec<String>,
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
        /// unstaged changes to tracked files.
        modified: Vec<String>,
    },
}

/// Where this tree's pin stands against `origin/main`'s pin at the merge-base. A bump moves
/// forward; a checkout whose submodule was not advanced moves it backward by recording the old
/// gitlink (`git commit -a`, `git add -A`, or a merge resolved there).
#[derive(Clone, Debug, PartialEq, Eq)]
enum PinMotion {
    /// The gitlink is the base's own, or there is nothing to compare with: no
    /// `origin/main`, or no gitlink at `vendor/astream` at the merge-base.
    Unchanged,
    /// The base's pin is an ancestor of this one: a bump.
    Forward,
    /// Neither pin is an ancestor of the other.
    Diverged,
    /// This pin is a strict ancestor of the base's: a regression. `declared` is the
    /// `astream-pin-rollback:` reason a commit on this branch gives, if any.
    Backward {
        base: String,
        declared: Option<String>,
    },
    /// The base's pin is not in the submodule's object store while this pin
    /// is, so this cannot descend from it: a stale checkout that never fetched
    /// what main moved on to.
    BaseNotFetched { base: String },
    /// Could not be decided, with the reason.
    Undecided(String),
}

/// Everything [`validate`] judges. [`capture`] reads it from the real
/// repository; the tests build a valid one and mutate it.
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
    /// Held as a result so that the git facts, which explain why cargo failed (an uninitialised
    /// submodule), are judged first.
    metadata: Result<Value, String>,
}

/// Validates that `vendor/astream` matches its pinned submodule obligation.
pub(crate) fn review(cargo: &crate::resolve::Cargo, root: &Path) -> Result<Option<Pin>, String> {
    if !root.join(SUBMODULE).exists() && !root.join(CONSUMER).exists() {
        return Ok(None);
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    let facts = capture(cargo, &root)?;
    validate(&root, &facts).map(Some)
}

fn capture(cargo: &crate::resolve::Cargo, root: &Path) -> Result<Facts, String> {
    let index = index(root)?;
    Ok(Facts {
        gitmodules: gitmodules(root)?,
        index: index.clone(),
        checkout: checkout(root),
        motion: pin_motion(root, &index),
        exclude: exclude(root)?,
        metadata: metadata(cargo, root),
    })
}

fn git(dir: &Path, args: &[&str], isolate: bool) -> Result<Vec<u8>, String> {
    let mut cmd = command(aterm_uds::os_path!("git"));
    cmd.args(args).current_dir(dir);
    if isolate {
        for var in REPO_ENV {
            cmd.env_remove(var);
        }
    }
    let out = cmd
        .output_owned()
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

fn checkout(root: &Path) -> Checkout {
    let dir = root.join(SUBMODULE);
    if !dir.is_dir() {
        return Checkout::Uninitialised("the directory does not exist".into());
    }
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

fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let mut cmd = command(aterm_uds::os_path!("git"));
    cmd.args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(dir);
    for var in REPO_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .output_owned()
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

/// Prefix of [`metadata`]'s refusal when the local Cargo cache lacks a locked crate.
pub(crate) const CACHE_LACKS_A_LOCKED_CRATE: &str =
    "this machine's Cargo cache lacks a crate the lock names";

/// Returns whether `why` is [`CACHE_LACKS_A_LOCKED_CRATE`]'s offline cache-miss refusal.
pub(crate) fn machine_refusal(why: &str) -> bool {
    why.starts_with(CACHE_LACKS_A_LOCKED_CRATE)
}

fn offline_cache_miss(stderr: &str) -> bool {
    stderr.contains("--offline was specified") || stderr.contains("you're using offline mode")
}

fn metadata(cargo: &crate::resolve::Cargo, root: &Path) -> Result<Value, String> {
    let out = cargo
        .command("metadata", root)
        .args([
            "--format-version",
            "1",
            "--all-features",
            "--locked",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .output_owned()
        .map_err(|e| format!("cannot read Cargo metadata: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if offline_cache_miss(&stderr) {
            return Err(format!(
                "{CACHE_LACKS_A_LOCKED_CRATE}, so the offline Cargo metadata cannot resolve the \
                 graph (one networked `targo fetch` fills the cache): {}",
                stderr.trim()
            ));
        }
        return Err(format!("Cargo metadata refused: {}", stderr.trim()));
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
    let crates = resolved_from_submodule(root, metadata)?;
    consumer_reaches_submodule(root, metadata)?;
    Ok(Pin {
        commit,
        url,
        crates,
    })
}

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

fn resolved_from_submodule(root: &Path, metadata: &Value) -> Result<Vec<String>, String> {
    let (packages, members) = packages_and_members(metadata)?;
    let under = root.join(SUBMODULE);
    let mut names: Vec<&str> = packages
        .iter()
        .filter(|p| {
            let named = p
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.starts_with(CRATE_PREFIX));
            let inside = p
                .get("manifest_path")
                .and_then(Value::as_str)
                .is_some_and(|m| Path::new(m).starts_with(&under));
            named || inside
        })
        .filter_map(|p| p.get("name").and_then(Value::as_str))
        .collect();
    names.sort_unstable();
    names.dedup();
    for &name in &names {
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
    Ok(names.into_iter().map(str::to_owned).collect())
}

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
    use aterm_json::json;

    #[test]
    fn an_offline_cache_miss_is_the_machines_refusal() {
        for stderr in [
            "error: failed to download `wgpu-core-deps-emscripten v29.0.3`\n\nCaused by:\n  \
             attempting to make an HTTP request, but --offline was specified",
            "error: no matching package named `x` found\nAs a reminder, you're using offline \
             mode (--offline) which can sometimes cause surprising resolution failures",
        ] {
            assert!(offline_cache_miss(stderr), "{stderr}");
        }
        assert!(!offline_cache_miss(
            "error: failed to parse manifest at `Cargo.toml`"
        ));
        assert!(machine_refusal(&format!(
            "{CACHE_LACKS_A_LOCKED_CRATE}, so …"
        )));
        assert!(!machine_refusal(
            "Cargo metadata refused: failed to parse manifest"
        ));
        let (root, mut facts) = valid();
        facts.metadata = Err(format!("{CACHE_LACKS_A_LOCKED_CRATE}, so …"));
        let why = validate(&root, &facts).expect_err("no graph, no pin");
        assert!(machine_refusal(&why), "{why}");
    }

    const FIXTURE: [&str; 6] = [
        "astream-broker",
        "astream-cap",
        "astream-digest",
        "astream-effects",
        "astream-term",
        "astream-wire",
    ];

    fn valid() -> (PathBuf, Facts) {
        let root = PathBuf::from("/w");
        let pin = "1".repeat(40);
        let path = |p: &Path| p.to_str().unwrap().to_owned();
        let id = |dir: &Path| format!("path+file://{}#0.1.0", path(dir));
        let crate_dir = |name: &str| root.join(SUBMODULE).join("crates").join(name);
        let mut packages: Vec<Value> = FIXTURE
            .iter()
            .map(|name| {
                let dir = crate_dir(name);
                json!({
                    "id": id(&dir),
                    "name": name,
                    "source": null,
                    "license": "Apache-2.0",
                    "manifest_path": path(&dir.join("Cargo.toml")),
                    "dependencies": [],
                })
            })
            .collect();
        let consumer = root.join(CONSUMER);
        let link = id(consumer.parent().unwrap());
        let deps: Vec<Value> = CONSUMER_DEPS
            .iter()
            .map(|name| json!({ "name": name, "path": path(&crate_dir(name)) }))
            .collect();
        packages.push(json!({
            "id": link.clone(),
            "name": "aterm-link",
            "source": null,
            "license": "Apache-2.0",
            "manifest_path": path(&consumer),
            "dependencies": deps,
        }));
        let facts = Facts {
            gitmodules: vec![
                (format!("submodule.{SUBMODULE}.path"), SUBMODULE.into()),
                (format!("submodule.{SUBMODULE}.url"), format!("{URL}.git")),
            ],
            index: vec![IndexEntry {
                mode: GITLINK_MODE.into(),
                object: pin.clone(),
                stage: "0".into(),
                path: SUBMODULE.into(),
            }],
            checkout: Checkout::At {
                head: pin,
                modified: Vec::new(),
            },
            motion: PinMotion::Unchanged,
            exclude: vec![SUBMODULE.into()],
            metadata: Ok(json!({ "packages": packages, "workspace_members": [link] })),
        };
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

    fn packages(facts: &mut Facts) -> &mut Vec<Value> {
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
    }

    fn set_url(facts: &mut Facts, url: &str) {
        for (k, v) in &mut facts.gitmodules {
            if k.ends_with(".url") {
                *v = url.to_string();
            }
        }
    }

    type Plant = Box<dyn Fn(&mut Facts)>;

    #[test]
    fn every_planted_drift_from_the_pinned_submodule_is_refused() {
        let (root, facts) = valid();
        let base = "0123456789012345678901234567890123456789".to_owned();
        let mut refused: Vec<(String, Plant)> = vec![
            (
                "no .gitmodules".into(),
                Box::new(|f: &mut Facts| f.gitmodules.clear()),
            ),
            (
                "another URL".into(),
                Box::new(|f: &mut Facts| set_url(f, "https://github.com/someone-else/astream.git")),
            ),
            (
                "two entries at the path".into(),
                Box::new(|f: &mut Facts| {
                    f.gitmodules
                        .push(("submodule.other.path".into(), SUBMODULE.into()));
                }),
            ),
            (
                "a tree, not a gitlink".into(),
                Box::new(|f: &mut Facts| f.index[0].mode = "100644".into()),
            ),
            (
                "a tracked file beneath the gitlink".into(),
                Box::new(|f: &mut Facts| {
                    f.index.push(IndexEntry {
                        mode: "100644".into(),
                        object: "0".repeat(40),
                        stage: "0".into(),
                        path: format!("{SUBMODULE}/crates/astream-cap/src/lib.rs"),
                    });
                }),
            ),
            (
                "no index entry".into(),
                Box::new(|f: &mut Facts| f.index.clear()),
            ),
            (
                "a checkout off the gitlink".into(),
                Box::new(|f: &mut Facts| {
                    f.checkout = Checkout::At {
                        head: "0".repeat(40),
                        modified: Vec::new(),
                    };
                }),
            ),
            (
                "an uninitialised submodule".into(),
                Box::new(|f: &mut Facts| f.checkout = Checkout::Uninitialised("empty".into())),
            ),
            (
                "a dirty checkout".into(),
                Box::new(|f: &mut Facts| {
                    if let Checkout::At { modified, .. } = &mut f.checkout {
                        modified.push(" M crates/astream-cap/src/lib.rs".into());
                    }
                }),
            ),
            (
                "an astream crate made a workspace member".into(),
                Box::new(|f: &mut Facts| {
                    let id = package(f, "astream-wire")["id"].clone();
                    f.metadata
                        .as_mut()
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .get_mut("workspace_members")
                        .unwrap()
                        .as_array_mut()
                        .unwrap()
                        .push(id);
                }),
            ),
            (
                "an astream crate from the registry".into(),
                Box::new(|f: &mut Facts| {
                    package(f, "astream-broker").insert(
                        "source".into(),
                        Value::String(
                            "registry+https://github.com/rust-lang/crates.io-index".into(),
                        ),
                    );
                }),
            ),
            (
                "an astream crate under another license".into(),
                Box::new(|f: &mut Facts| {
                    package(f, "astream-digest")
                        .insert("license".into(), Value::String("MIT".into()));
                }),
            ),
            (
                "an astream crate resolved twice".into(),
                Box::new(|f: &mut Facts| {
                    let copy = Value::Object(package(f, "astream-cap").clone());
                    packages(f).push(copy);
                }),
            ),
            (
                "cargo metadata refused".into(),
                Box::new(|f: &mut Facts| {
                    f.metadata = Err("Cargo metadata refused: planted".into())
                }),
            ),
            (
                "no workspace exclude".into(),
                Box::new(|f: &mut Facts| f.exclude.clear()),
            ),
            (
                "only a sibling excluded".into(),
                Box::new(|f: &mut Facts| {
                    f.exclude = vec!["vendor/astream-other".into(), "vendor/ast".into()];
                }),
            ),
            (
                "aterm-link redirected away".into(),
                Box::new(|f: &mut Facts| {
                    let deps = package(f, "aterm-link")
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
                }),
            ),
            (
                "an undeclared backward pin".into(),
                Box::new({
                    let base = base.clone();
                    move |f: &mut Facts| {
                        f.motion = PinMotion::Backward {
                            base: base.clone(),
                            declared: None,
                        };
                    }
                }),
            ),
            (
                "a base the checkout never fetched".into(),
                Box::new({
                    let base = base.clone();
                    move |f: &mut Facts| f.motion = PinMotion::BaseNotFetched { base: base.clone() }
                }),
            ),
        ];
        for name in FIXTURE {
            refused.push((
                format!("{name} redirected"),
                Box::new(move |f: &mut Facts| {
                    package(f, name).insert(
                        "manifest_path".into(),
                        Value::String("/outside/Cargo.toml".into()),
                    );
                }),
            ));
        }
        for (name, plant) in &refused {
            let mut planted = facts.clone();
            plant(&mut planted);
            assert!(validate(&root, &planted).is_err(), "{name} must be refused");
        }

        let mut accepted: Vec<Facts> = Vec::new();
        for spelling in [URL.to_string(), format!("{URL}.git")] {
            let mut f = facts.clone();
            set_url(&mut f, &spelling);
            accepted.push(f);
        }
        let mut f = facts.clone();
        f.exclude = vec!["vendor/".into()];
        accepted.push(f);
        for motion in [
            PinMotion::Backward {
                base,
                declared: Some("astream 5021230 broke the sealed wire".into()),
            },
            PinMotion::Unchanged,
            PinMotion::Forward,
            PinMotion::Diverged,
            PinMotion::Undecided("no origin/main".into()),
        ] {
            let mut f = facts.clone();
            f.motion = motion;
            accepted.push(f);
        }
        for f in &accepted {
            assert!(validate(&root, f).is_ok(), "{f:?} must pass");
        }
    }

    #[test]
    fn an_astream_crate_no_list_names_is_held_to_the_submodule_too() {
        let (root, facts) = valid();
        assert_eq!(validate(&root, &facts).unwrap().crates, FIXTURE);
        let add = |facts: &mut Facts, name: &str, manifest: &Path| {
            let id = format!("path+file://{}#0.1.0", manifest.parent().unwrap().display());
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
                .push(json!({
                    "id": id,
                    "name": name,
                    "source": null,
                    "license": "Apache-2.0",
                    "manifest_path": manifest.to_str().unwrap(),
                    "dependencies": [],
                }));
        };
        let at_pin = |name: &str| {
            root.join(SUBMODULE)
                .join("crates")
                .join(name)
                .join("Cargo.toml")
        };
        let mut sibling = facts.clone();
        let elsewhere = Path::new("/astream/crates/astream-next/Cargo.toml");
        add(&mut sibling, "astream-next", elsewhere);
        rejects(
            &root,
            &sibling,
            &["redirects `astream-next`", "/astream/crates/astream-next"],
        );
        let mut licensed = facts.clone();
        add(&mut licensed, "astream-next", &at_pin("astream-next"));
        package(&mut licensed, "astream-next")
            .insert("license".into(), Value::String("MIT".into()));
        rejects(
            &root,
            &licensed,
            &["`astream-next`", "declares license MIT"],
        );
        let mut pinned = facts;
        add(&mut pinned, "astream-next", &at_pin("astream-next"));
        add(&mut pinned, "asb", &at_pin("asb"));
        let crates = validate(&root, &pinned)
            .expect("both resolve from the pin")
            .crates;
        assert!(
            ["asb", "astream-next"]
                .iter()
                .all(|n| crates.iter().any(|c| c == n)),
            "{crates:?}"
        );
        let id = package(&mut pinned, "asb")["id"].clone();
        pinned
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
        rejects(&root, &pinned, &["`asb` is an aterm workspace member"]);
    }

    struct Fixture {
        root: aterm_tempfile::TempDir,
    }

    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let root = aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-forge-pin-{tag}-"))
                .tempdir()
                .unwrap();
            std::fs::create_dir_all(root.path().join(SUBMODULE)).unwrap();
            let f = Fixture { root };
            f.run(f.root.path(), &["init", "-q", "-b", "main"]);
            f.run(
                &f.root.path().join(SUBMODULE),
                &["init", "-q", "-b", "main"],
            );
            f
        }

        fn run(&self, dir: &Path, args: &[&str]) -> String {
            let mut cmd = command(aterm_uds::os_path!("git"));
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
            for var in REPO_ENV {
                cmd.env_remove(var);
            }
            let out = cmd.output_owned().unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        fn astream_commit(&self, parent: Option<&str>, msg: &str) -> String {
            let sub = self.root.path().join(SUBMODULE);
            if let Some(p) = parent {
                self.run(&sub, &["checkout", "-q", "--detach", p]);
            }
            self.run(&sub, &["commit", "-q", "--allow-empty", "-m", msg]);
            self.run(&sub, &["rev-parse", "HEAD"])
        }

        fn commit_pin(&self, parent: Option<&str>, pin: &str, msg: &str) -> String {
            if let Some(p) = parent {
                self.run(self.root.path(), &["checkout", "-q", "--detach", p]);
            }
            let cacheinfo = format!("160000,{pin},{SUBMODULE}");
            self.run(
                self.root.path(),
                &["update-index", "--add", "--cacheinfo", &cacheinfo],
            );
            self.run(self.root.path(), &["commit", "-q", "-m", msg]);
            self.run(self.root.path(), &["rev-parse", "HEAD"])
        }

        fn motion(&self) -> PinMotion {
            for var in REPO_ENV {
                assert!(
                    std::env::var_os(var).is_none(),
                    "{var} is exported: unset it, or this test reads a real repository"
                );
            }
            pin_motion(self.root.path(), &index(self.root.path()).unwrap())
        }
    }

    #[test]
    fn the_pin_is_judged_against_origin_main_at_the_merge_base_with_real_git() {
        let f = Fixture::new("motion");
        let c1 = f.astream_commit(None, "c1");
        let c2 = f.astream_commit(None, "c2");
        let c3 = f.astream_commit(None, "c3");
        let sibling = f.astream_commit(Some(&c1), "sibling of c2");

        let base = f.commit_pin(None, &c2, "base: the pin is c2");
        assert_eq!(
            f.motion(),
            PinMotion::Unchanged,
            "no {BASE_REF} to judge by"
        );
        f.run(
            f.root.path(),
            &["update-ref", "refs/remotes/origin/main", &base],
        );

        assert_eq!(f.motion(), PinMotion::Unchanged);

        f.commit_pin(Some(&base), &c3, "bump to c3");
        assert_eq!(f.motion(), PinMotion::Forward);

        f.commit_pin(Some(&base), &c1, "an unrelated fix that drags the pin back");
        assert_eq!(
            f.motion(),
            PinMotion::Backward {
                base: c2.clone(),
                declared: None
            }
        );

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
        f.run(
            f.root.path(),
            &["commit", "-q", "--allow-empty", "-m", "adapt aterm-link"],
        );
        assert_eq!(
            f.motion(),
            declared,
            "the trailer is not confined to the tip"
        );
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

        f.commit_pin(Some(&base), &sibling, "sideways to a sibling of c2");
        assert_eq!(f.motion(), PinMotion::Diverged);

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
        let unfetched = "a".repeat(40);
        let base = f.commit_pin(None, &unfetched, "main: pinned to a commit not held here");
        f.run(
            f.root.path(),
            &["update-ref", "refs/remotes/origin/main", &base],
        );
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
        std::fs::write(f.root.path().join("README"), "x").unwrap();
        f.run(f.root.path(), &["add", "README"]);
        f.run(
            f.root.path(),
            &["commit", "-q", "-m", "base: no submodule yet"],
        );
        let base = f.run(f.root.path(), &["rev-parse", "HEAD"]);
        f.run(
            f.root.path(),
            &["update-ref", "refs/remotes/origin/main", &base],
        );
        f.commit_pin(Some(&base), &c1, "add the submodule");
        assert_eq!(f.motion(), PinMotion::Unchanged);
    }

    #[test]
    fn a_local_branch_named_origin_main_is_not_the_base() {
        let f = Fixture::new("shadow");
        let c1 = f.astream_commit(None, "c1");
        let c2 = f.astream_commit(None, "c2");
        let base = f.commit_pin(None, &c2, "the pin is c2");
        f.run(f.root.path(), &["branch", "origin/main", &base]);
        f.commit_pin(Some(&base), &c1, "goes back to c1");
        assert_eq!(f.motion(), PinMotion::Unchanged);
    }
}
