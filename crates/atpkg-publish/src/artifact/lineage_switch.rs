// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lineage receipt switch: whether a compiler with no LINEAGE or SEED-MINT receipt may
//! still be admitted (recorded `lineage = "unproven"`), read from the trust repo's live
//! origin main.
//!
//! Dormant only when the seed ledger ([`LEDGER`]) at the live main is readable and has no
//! `[release.mint]`; anything unknown is not dormant. A mint on the local origin/main is
//! live with no network; otherwise `git ls-remote` names the live main, fetched if absent
//! into a quarantine object dir that moves no ref. Every network step is bounded by
//! [`super::tools::HANG`].
//!
//! Read by every pack (and again before an `unproven` row is signed), `sign`, the group
//! publish and the Linux lane.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::ctx::Ctx;
use crate::error::Result;

/// What the switch reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Switch {
    /// No minted re-pin on the trust repo's live main: an unproven compiler is admitted
    /// (and recorded so).
    Dormant,
    /// A minted re-pin is there: every compiler needs its receipt.
    Live,
    /// Nothing shows either (no checkout, no answer, no readable ledger): required. The
    /// string says why, and what to fetch where a fetch is the remedy.
    Unknown(String),
}

impl Switch {
    /// The client checker's policy this reading is: dormant admits an unproven compiler;
    /// live and unknown both require the receipt.
    #[must_use]
    pub fn policy(&self) -> atpkg::lineage::LineagePolicy {
        match self {
            Self::Dormant => atpkg::lineage::LineagePolicy::Dormant,
            Self::Live | Self::Unknown(_) => atpkg::lineage::LineagePolicy::Require,
        }
    }
}

/// The trust checkout the switch is read from: [`Ctx::trust_repo`] (`$HOME/trust` unless the
/// verb's `--trust-repo` names another).
#[must_use]
pub fn default_trust_repo(ctx: &Ctx) -> PathBuf {
    ctx.trust_repo.clone()
}

/// Where the ledger lives in the trust repo.
pub const LEDGER: &str = "bootstrap/trust-stage0/seed-ledger.toml";

/// The bound on every network step (a hang detector).
const NETWORK_BOUND: Duration = super::tools::HANG;

/// A quarantine left by a run killed outright is swept once it is this old: no live
/// fetch outlasts its [`NETWORK_BOUND`].
const QUARANTINE_STALE: Duration = Duration::from_secs(3600);

const QUARANTINE_PREFIX: &str = "atpkg-lineage-fetch.";

/// Read the switch from `trust_repo`'s live origin main.
///
/// # Errors
///
/// None: every failure to know is [`Switch::Unknown`], which callers read as required.
pub fn read(ctx: &Ctx, trust_repo: &Path) -> Result<Switch> {
    Ok(read_switch(ctx, trust_repo))
}

fn read_switch(ctx: &Ctx, repo: &Path) -> Switch {
    let shown = ctx.shown(repo);
    if repo.as_os_str().is_empty() || !repo.is_dir() {
        return Switch::Unknown(format!(
            "there is no trust checkout at {shown} to read it from; clone the trust repo \
             there and pack again"
        ));
    }
    let fetch = format!("git -C {shown} fetch origin main");
    let Some(here) = git_ok(
        repo,
        &[
            "rev-parse",
            "-q",
            "--verify",
            "refs/remotes/origin/main^{commit}",
        ],
    ) else {
        return Switch::Unknown(format!(
            "{shown} has no origin/main; run `{fetch}` and pack again"
        ));
    };
    match ledger_at(repo, &here) {
        Ledger::Dormant => {}
        Ledger::Minted => return Switch::Live,
        Ledger::Unreadable => {
            return Switch::Unknown(format!(
                "{shown}'s origin/main ({}) has no readable {LEDGER}",
                short(&here)
            ));
        }
    }
    let required = |why: String| {
        Switch::Unknown(format!(
            "{shown}'s origin/main ({}) carries no minted re-pin, but {why}, so nothing shows \
             the switch is still dormant; run `{fetch}` and pack again",
            short(&here)
        ))
    };
    let live = match bounded_git(repo, &[], &["ls-remote", "origin", "refs/heads/main"]) {
        Bounded::TimedOut => {
            return required(format!(
                "its origin did not answer `git ls-remote` within {} s",
                NETWORK_BOUND.as_secs()
            ));
        }
        Bounded::Failed(why) => return required(format!("`git ls-remote origin` failed ({why})")),
        Bounded::Ok(out) => out.lines().find_map(|l| {
            let (sha, r) = l.split_once(char::is_whitespace)?;
            (r.trim() == "refs/heads/main").then(|| sha.to_string())
        }),
    };
    let Some(live) = live else {
        return required("its origin has no main".into());
    };
    if live == here {
        return Switch::Dormant;
    }
    if !has_commit(repo, &live) {
        match quarantined_fetch(repo) {
            Ok(()) => {}
            Err(Bounded::TimedOut) => {
                return required(format!(
                    "its origin's main is {} now, and fetching it did not finish within {} s",
                    short(&live),
                    NETWORK_BOUND.as_secs()
                ));
            }
            Err(Bounded::Failed(why) | Bounded::Ok(why)) => {
                return required(format!(
                    "its origin's main is {} now, and fetching it failed ({why})",
                    short(&live)
                ));
            }
        }
    }
    let ledger = ledger_at(repo, &live);
    note_live(repo, &live);
    match ledger {
        Ledger::Dormant => Switch::Dormant,
        Ledger::Minted => Switch::Live,
        Ledger::Unreadable => required(format!(
            "its origin's main ({}) has no readable seed ledger here",
            short(&live)
        )),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

enum Ledger {
    Dormant,
    Minted,
    Unreadable,
}

fn ledger_at(repo: &Path, commit: &str) -> Ledger {
    match git_ok(repo, &["show", &format!("{commit}:{LEDGER}")]) {
        Some(text) if !text.is_empty() => {
            if text.lines().any(|l| l.starts_with("[release.mint]")) {
                Ledger::Minted
            } else {
                Ledger::Dormant
            }
        }
        _ => Ledger::Unreadable,
    }
}

fn has_commit(repo: &Path, sha: &str) -> bool {
    git_ok(repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_some()
}

/// A local git question: its stdout when it succeeds.
fn git_ok(repo: &Path, args: &[&str]) -> Option<String> {
    crate::git::run(repo, args).ok()
}

enum Bounded {
    Ok(String),
    Failed(String),
    TimedOut,
}

fn bounded_git(repo: &Path, env: &[(&str, &std::ffi::OsStr)], args: &[&str]) -> Bounded {
    let mut cmd = aterm_uds::spawnfd::command("git");
    cmd.arg("-C").arg(repo).args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for (k, v) in env {
        cmd.env(k, v);
    }
    match aterm_uds::deadline::output_until(&mut cmd, Instant::now() + NETWORK_BOUND) {
        Ok(out) if out.status.success() => Bounded::Ok(String::from_utf8_lossy(&out.stdout).into()),
        Ok(out) => Bounded::Failed(format!(
            "{}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no message")
        )),
        Err(aterm_uds::deadline::RunError::TimedOut) => Bounded::TimedOut,
        Err(e) => Bounded::Failed(format!("{e:?}")),
    }
}

/// `<git-dir>/atpkg-lineage-live`: the last live main a check fetched — a plain file, not
/// a ref (no ref moves), offered as the next fetch's negotiation tip.
fn note_path(repo: &Path) -> Option<PathBuf> {
    let p = git_ok(repo, &["rev-parse", "--git-path", "atpkg-lineage-live"])?;
    let p = PathBuf::from(p);
    Some(if p.is_absolute() { p } else { repo.join(p) })
}

/// Best effort: a note that cannot be written only costs the next check a larger fetch.
fn note_live(repo: &Path, live: &str) {
    if !has_commit(repo, live) {
        return;
    }
    let Some(note) = note_path(repo) else {
        return;
    };
    if std::fs::read_to_string(&note).is_ok_and(|s| s.lines().next() == Some(live)) {
        return;
    }
    let tmp = note.with_extension(format!("{}", std::process::id()));
    if std::fs::write(&tmp, format!("{live}\n")).is_err() || std::fs::rename(&tmp, &note).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A quarantine directory removed however the fetch ends.
struct Quarantine(PathBuf);

impl Drop for Quarantine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Fetch origin's main into `repo`'s object store and nothing else: the objects land in a
/// quarantine (the store its alternate) and move in only once the fetch finished.
fn quarantined_fetch(repo: &Path) -> std::result::Result<(), Bounded> {
    let fail = |why: &str| Bounded::Failed(why.to_string());
    let od = git_ok(repo, &["rev-parse", "--git-path", "objects"])
        .ok_or_else(|| fail("no object store"))?;
    let od = PathBuf::from(od);
    let od = if od.is_absolute() { od } else { repo.join(od) };
    let od = std::fs::canonicalize(&od).map_err(|e| fail(&e.to_string()))?;
    let mut tips = vec!["--negotiation-tip=refs/remotes/origin/main".to_string()];
    if let Some(noted) = note_path(repo)
        .and_then(|n| std::fs::read_to_string(n).ok())
        .and_then(|s| s.lines().next().map(str::to_string))
        .filter(|s| is_sha(s) && has_commit(repo, s))
    {
        tips.push(format!("--negotiation-tip={noted}"));
    }
    sweep_quarantines(&od);
    let q = Quarantine(od.join(format!(
        "{QUARANTINE_PREFIX}{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    )));
    std::fs::create_dir(&q.0).map_err(|e| fail(&e.to_string()))?;
    let mut alternates = od.clone().into_os_string();
    if let Some(more) =
        std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES").filter(|v| !v.is_empty())
    {
        alternates.push(":");
        alternates.push(more);
    }
    let mut args = vec![
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "-c",
        "fetch.writeCommitGraph=false",
        "fetch",
        "-q",
        "--no-write-fetch-head",
        "--no-tags",
        "--no-recurse-submodules",
        "--refmap=",
    ];
    args.extend(tips.iter().map(String::as_str));
    args.extend(["origin", "refs/heads/main"]);
    match bounded_git(
        repo,
        &[
            ("GIT_OBJECT_DIRECTORY", q.0.as_os_str()),
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES", &alternates),
        ],
        &args,
    ) {
        Bounded::Ok(_) => adopt(&q.0, &od).map_err(|e| fail(&format!("adopting the fetch: {e}"))),
        other => Err(other),
    }
}

/// Remove quarantines a killed run left, once they are an hour old. The store's own
/// `pack/` is never swept: a partial pack cannot be told from a concurrent fetch's.
fn sweep_quarantines(od: &Path) {
    let Ok(entries) = std::fs::read_dir(od) else {
        return;
    };
    for e in entries.flatten() {
        let stale = e
            .file_name()
            .to_string_lossy()
            .starts_with(QUARANTINE_PREFIX)
            && e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > QUARANTINE_STALE);
        if stale {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Move what the quarantine stored into the store the way git's own tmp-objdir migrates:
/// each pack's other files before its `.idx` (no reader ever sees an index whose pack is
/// not there yet), loose objects by rename, nothing already in the store replaced.
fn adopt(q: &Path, od: &Path) -> std::io::Result<()> {
    let qpack = q.join("pack");
    if qpack.is_dir() {
        std::fs::create_dir_all(od.join("pack"))?;
        let mut names: Vec<String> = std::fs::read_dir(&qpack)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for idx in names
            .iter()
            .filter(|n| n.starts_with("pack-") && n.ends_with(".idx"))
        {
            let base = idx.trim_end_matches(".idx");
            if od.join("pack").join(idx).exists() {
                continue;
            }
            for other in names
                .iter()
                .filter(|n| *n != idx && n.strip_prefix(base).is_some_and(|r| r.starts_with('.')))
            {
                std::fs::rename(qpack.join(other), od.join("pack").join(other))?;
            }
            std::fs::rename(qpack.join(idx), od.join("pack").join(idx))?;
        }
    }
    for e in std::fs::read_dir(q)?.flatten() {
        let dir = e.file_name().to_string_lossy().into_owned();
        if dir.len() != 2 || !dir.bytes().all(|b| b.is_ascii_hexdigit()) || !e.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(e.path())?.flatten() {
            let n = f.file_name().to_string_lossy().into_owned();
            let loose = matches!(n.len(), 38 | 62) && n.bytes().all(|b| b.is_ascii_hexdigit());
            let dest = od.join(&dir).join(&n);
            if loose && f.path().is_file() && !dest.exists() {
                std::fs::create_dir_all(od.join(&dir))?;
                std::fs::rename(f.path(), dest)?;
            }
        }
    }
    Ok(())
}
