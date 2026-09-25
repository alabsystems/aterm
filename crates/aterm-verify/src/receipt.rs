// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE GATE DECIDED ABOUT ONE TREE, written where a hook can read it in
//! milliseconds.
//!
//! WHY (2026-09-17). aterm has no CI by owner decision: the merge contract is
//! `tools/verify.sh --fast` passing locally before a slice enters main. The
//! only thing standing between an ungated commit and `origin/main` was
//! `.githooks/pre-push`, and that hook has been ADVISORY since 2026-08-24 — it
//! prints a suggestion and pushes anyway. Three defects shipped green through
//! that gap in August alone. So "the push gate" named a thing that did not
//! exist, and the repo's own answer was to correct the SENTENCE in twenty
//! places rather than the gap.
//!
//! The hook was demoted for two measured reasons, and both are about RUNNING
//! THE GATE inside the hook:
//!
//!  * it took twelve minutes once `tools/paint_guard.sh` joined the lane, and
//!    "a hook slow enough to be bypassed is worse than none";
//!  * it could not WIN — `main` advanced twice during one gate, so a green
//!    verdict still ended in `[remote rejected] cannot lock ref`.
//!
//! Neither is a reason a hook cannot CHECK A RESULT. A receipt is the result:
//! the gate already knows the tree it verified (`identity::TreeState`) and
//! whether that run discharged the whole merge contract
//! ([`crate::verdict::Verdict::claims_merge_contract`]), so it writes both,
//! keyed by the commit, into the gate-state directory. The hook reads one
//! small file per pushed ref and decides in microseconds. It never compiles,
//! never races another push, and cannot teach the bypass.
//!
//! WHAT A RECEIPT IS NOT. It is a record the gate wrote, not evidence anyone
//! can check — the same standing the snapshot marker has, and for the same
//! reason (`crate::snapshot::marker_state`): whoever can write a file can write
//! one. It stops the accident it is about — pushing a commit no gate ever ran
//! on — and claims nothing against someone editing their own state directory.
//!
//! ONLY A CLEAN TREE GETS ONE. A run over HEAD plus uncommitted work verified
//! bytes no commit holds, so it records nothing: its receipt could only ever
//! refuse, and keyed by HEAD it would stand where HEAD's own receipt belongs.
//!
//! ONE RECEIPT STORE PER REPOSITORY: receipts live under the git COMMON dir
//! ([`dir`]), which every worktree of a repository shares, so a PASS written
//! from the worktree an agent gated in reaches the checkout that pushes.
//!
//! A WEAKER RECEIPT NEVER REPLACES A WHOLE-TREE ONE ([`write`]). The file is
//! keyed by commit and the store is shared, so without this a `--changed` or
//! `--scope` run, or a flaky FAIL, on a commit that already carries a
//! merge-contract receipt — from any worktree sitting at that commit —
//! destroyed the one receipt that admits it. A whole-tree PASS on these exact
//! bytes is a fact no later run unmakes; among receipts that admit nothing,
//! the newest wins.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Where receipts live: `<git common dir>/`[`RECEIPT_DIR`].
pub const RECEIPT_DIR: &str = "aterm-verify/receipts";

/// The first line of every receipt. A reader that does not recognise it must
/// treat the file as no receipt at all, never as a permissive one.
///
/// Format 2 (2026-09-23) dropped `tree` — only a clean tree gets a receipt —
/// and `base`. A format-1 file is not a receipt: it admits nothing, and the
/// hook says so in one sentence.
pub const MAGIC: &str = "aterm-verify receipt 2";

/// How many receipts are kept. One store serves every worktree of the
/// repository, so this covers days of gating; a receipt is a few hundred
/// bytes.
pub const KEPT: usize = 1000;

/// One run's verdict about one commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// The commit the run verified, on a clean tree.
    pub head: String,
    /// `fast` / `full` / `changed:<base>` — whatever the run called itself.
    pub mode: String,
    /// `workspace`, `crate:<name>`, or `changed`.
    pub scope: String,
    /// `PASS`, `FAIL` or `COULD-NOT-RUN`.
    pub verdict: String,
    /// Did this run discharge the WHOLE merge contract? The one predicate a
    /// push may be decided on, and it is [`crate::verdict::claims_contract`]'s,
    /// not a second opinion: whole tree, nothing skipped, nothing failed, not a
    /// selftest.
    pub merge_contract: bool,
    /// What this run SKIPPED, in the verdict's own words (at most a handful,
    /// then a count) — never part of the decision, which `merge_contract`
    /// already carries, but the difference between "the gate refused you" and
    /// "the gate could not present on this machine", which is what an operator
    /// on a headless box needs to read.
    pub skipped: String,
    /// Seconds since the epoch, for the operator — never for the decision.
    pub when: u64,
}

impl Receipt {
    /// The file a hook reads. One `key value` per line, so `grep` is enough and
    /// no hook needs a parser.
    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::from(MAGIC);
        s.push('\n');
        let _ = writeln!(s, "head {}", self.head);
        let _ = writeln!(s, "mode {}", self.mode);
        let _ = writeln!(s, "scope {}", self.scope);
        let _ = writeln!(s, "verdict {}", self.verdict);
        let _ = writeln!(
            s,
            "merge-contract {}",
            if self.merge_contract { "yes" } else { "no" }
        );
        let _ = writeln!(s, "skipped {}", self.skipped);
        let _ = writeln!(s, "when {}", self.when);
        s
    }

    /// Read one back. `None` for anything that is not a receipt this version
    /// wrote — an unknown format is NOT a receipt, so it can never admit a push.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let mut get = std::collections::BTreeMap::new();
        for l in lines {
            if let Some((k, v)) = l.split_once(' ') {
                get.insert(k.to_string(), v.to_string());
            }
        }
        Some(Self {
            head: get.get("head")?.clone(),
            mode: get.get("mode")?.clone(),
            scope: get.get("scope")?.clone(),
            verdict: get.get("verdict")?.clone(),
            merge_contract: get.get("merge-contract").is_some_and(|v| v == "yes"),
            skipped: get.get("skipped").cloned().unwrap_or_default(),
            when: get.get("when").and_then(|w| w.parse().ok()).unwrap_or(0),
        })
    }
}

/// `<git common dir of root>/aterm-verify/receipts` — the one receipt store
/// every worktree of the repository shares, and the one `.githooks/pre-push`
/// reads (it asks git the same question).
///
/// # Errors
/// When git cannot say where `root`'s common dir is: not a repository, or git
/// failing. No receipt is written then, which costs the next push a refusal.
pub fn dir(root: &Path) -> std::io::Result<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()?;
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || common.is_empty() {
        return Err(std::io::Error::other(format!(
            "git cannot name the common git dir of {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(PathBuf::from(common).join(RECEIPT_DIR))
}

/// What [`write`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    /// This receipt now stands at the path.
    Stored(PathBuf),
    /// The commit already carries a merge-contract receipt at the path, and
    /// this one does not discharge the contract, so the stronger one stayed.
    KeptWholeTree(PathBuf),
}

/// Write `r` under `root`, keyed by the commit it is about — unless that
/// commit already carries a merge-contract receipt and `r` does not.
///
/// The check and the replace happen under an exclusive lock on
/// `<store>.lock`, so two runs finishing together cannot interleave a weaker
/// write past the check.
///
/// Best effort in one direction only: a receipt that cannot be written costs
/// the NEXT push a refusal, never this run its verdict. The gate says so on
/// stderr rather than failing, because a read-only state directory is an
/// operator's problem with their disk and not a finding about their change.
pub fn write(root: &Path, r: &Receipt) -> std::io::Result<Written> {
    let dir = dir(root)?;
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.with_extension("lock"))?;
    lock.lock()?;
    let path = dir.join(&r.head);
    if !r.merge_contract
        && std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| Receipt::parse(&text))
            .is_some_and(|standing| standing.merge_contract)
    {
        return Ok(Written::KeptWholeTree(path));
    }
    prune(&dir);
    // Whole-file replace: a half-written receipt that still parsed would be the
    // one failure mode that matters here.
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, r.render())?;
    std::fs::rename(&tmp, &path)?;
    Ok(Written::Stored(path))
}

/// Keep the newest [`KEPT`] receipts. Housekeeping only: every failure here is
/// ignored, because a full directory must never cost a run its verdict.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if kept.len() < KEPT {
        return;
    }
    kept.sort_unstable();
    let drop_n = kept.len() + 1 - KEPT;
    for (_, p) in kept.into_iter().take(drop_n) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn green() -> Receipt {
        Receipt {
            head: "a".repeat(40),
            mode: "fast".into(),
            scope: "workspace".into(),
            verdict: "PASS".into(),
            merge_contract: true,
            skipped: "none".into(),
            when: 1_758_000_000,
        }
    }

    /// A whole-tree receipt and a narrowed one both round-trip.
    #[test]
    fn a_receipt_round_trips() {
        let r = green();
        assert_eq!(Receipt::parse(&r.render()), Some(r.clone()));
        let changed = Receipt {
            scope: "changed".into(),
            merge_contract: false,
            ..green()
        };
        assert_eq!(Receipt::parse(&changed.render()), Some(changed));
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=gate",
                "-c",
                "user.email=gate@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs")
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A scratch repository with one commit: `(scratch dir, checkout)`.
    fn repo(tag: &str) -> (PathBuf, PathBuf) {
        let tmp = crate::mktemp_dir(tag).expect("mktemp");
        let main = tmp.join("main");
        std::fs::create_dir_all(&main).expect("mkdir");
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "one"]);
        (tmp, main)
    }

    fn stored(w: Written) -> PathBuf {
        match w {
            Written::Stored(path) => path,
            Written::KeptWholeTree(path) => panic!("kept instead of stored: {}", path.display()),
        }
    }

    /// ONE STORE PER REPOSITORY: a receipt written from a linked worktree lands
    /// where the main checkout reads, because both resolve the same git common
    /// dir. The negative control is the per-checkout path this replaced, which
    /// two worktrees never shared.
    #[test]
    fn every_worktree_of_a_repository_shares_one_receipt_store() {
        let (tmp, main) = repo("atv-receipt-common");
        let linked = tmp.join("linked");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().expect("utf-8"),
                "-b",
                "side",
            ],
        );
        let from_linked = dir(&linked).expect("the linked worktree's store");
        assert_eq!(from_linked, dir(&main).expect("the main checkout's store"));
        let written = stored(write(&linked, &green()).expect("written from the linked worktree"));
        assert!(
            written.starts_with(dir(&main).expect("store")),
            "{}",
            written.display()
        );
        assert!(
            !written.starts_with(&linked),
            "the negative control: a per-checkout store would sit inside the worktree"
        );
        assert!(
            dir(&tmp.join("nowhere")).is_err(),
            "no repository, no store"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// AN UNREADABLE RECEIPT IS NO RECEIPT. The one direction this must never
    /// fail in: a file the hook cannot understand must refuse the push, never
    /// wave it through.
    #[test]
    fn anything_that_is_not_this_format_parses_as_nothing() {
        let body = "head x\nmode fast\nscope workspace\nverdict PASS\nmerge-contract yes\n\
                    skipped none\nwhen 1\n";
        for text in [
            String::new(),
            // Format 1, whole and passing: an older gate's, so no receipt.
            format!("aterm-verify receipt 1\nhead x\ntree clean\n{}", &body[7..]),
            format!("aterm-verify receipt 3\n{body}"),
            body.to_string(),
            format!("{MAGIC}\nhead x\n"),
        ] {
            assert_eq!(Receipt::parse(&text), None, "{text:?}");
        }
        assert!(
            Receipt::parse(&format!("{MAGIC}\n{body}")).is_some(),
            "the control: the same body under this format's magic is a receipt"
        );
    }

    /// A WEAKER RECEIPT NEVER REPLACES A WHOLE-TREE ONE. The store is shared
    /// and keyed by commit, so a `--changed` run, a `--scope` run or a flaky
    /// FAIL at a fully receipted commit — from any worktree at that commit —
    /// would otherwise destroy the receipt that admits it. The negative
    /// controls are the replacements that must still happen: last writer wins
    /// among receipts that admit nothing, a whole-tree receipt replaces a
    /// narrowed one, and a whole-tree receipt refreshes itself.
    #[test]
    fn a_weaker_receipt_never_replaces_a_whole_tree_one() {
        let (tmp, main) = repo("atv-receipt-keep");
        let read = || {
            let path = dir(&main).expect("store").join("a".repeat(40));
            Receipt::parse(&std::fs::read_to_string(path).expect("a receipt stands"))
                .expect("it parses")
        };
        let changed = Receipt {
            scope: "changed".into(),
            merge_contract: false,
            when: 2,
            ..green()
        };
        let scoped = Receipt {
            scope: "crate:aterm-grid".into(),
            merge_contract: false,
            when: 3,
            ..green()
        };
        let failed = Receipt {
            verdict: "FAIL".into(),
            merge_contract: false,
            when: 4,
            ..green()
        };

        // Nothing admits yet: the newest narrowed run wins.
        stored(write(&main, &changed).expect("write"));
        stored(write(&main, &scoped).expect("write"));
        assert_eq!(
            read(),
            scoped,
            "last writer wins among receipts that admit nothing"
        );

        // A whole-tree receipt replaces a narrowed one…
        stored(write(&main, &green()).expect("write"));
        assert_eq!(read(), green());
        // …and nothing weaker replaces it.
        for weaker in [&changed, &scoped, &failed] {
            assert!(
                matches!(
                    write(&main, weaker).expect("write"),
                    Written::KeptWholeTree(_)
                ),
                "{weaker:?}"
            );
            assert_eq!(
                read(),
                green(),
                "{weaker:?} replaced the whole-tree receipt"
            );
        }
        // A newer whole-tree receipt refreshes it.
        let refreshed = Receipt { when: 5, ..green() };
        stored(write(&main, &refreshed).expect("write"));
        assert_eq!(read(), refreshed);
        // The lock sits beside the store, never among the receipts.
        let store = dir(&main).expect("store");
        assert!(store.with_extension("lock").is_file());
        let names: Vec<String> = std::fs::read_dir(&store)
            .expect("store")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a".repeat(40)]);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
