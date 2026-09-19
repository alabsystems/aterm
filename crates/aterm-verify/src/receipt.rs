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
//! The tree is recorded as CLEAN or with its dirty digest, because the two are
//! different claims: a gate run over HEAD plus uncommitted work has verified
//! bytes that are not the bytes being pushed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Where receipts live under [`crate::identity::GATE_STATE_DIR`].
pub const RECEIPT_DIR: &str = "receipts";

/// The first line of every receipt. A reader that does not recognise it must
/// treat the file as no receipt at all, never as a permissive one.
pub const MAGIC: &str = "aterm-verify receipt 1";

/// How many receipts are kept. Enough to cover a day of rebasing; a receipt is
/// worthless the moment its commit is gone.
pub const KEPT: usize = 200;

/// One run's verdict about one commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// The commit the run verified.
    pub head: String,
    /// The dirty digest of the working tree the run verified, or `None` for a
    /// clean tree — the ONLY state in which what was verified is what a push
    /// would send.
    pub dirty: Option<String>,
    /// `fast` / `full` / `changed:<base>` — whatever the run called itself.
    pub mode: String,
    /// `workspace`, or the crate a narrowed run was about.
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
        match &self.dirty {
            None => s.push_str("tree clean\n"),
            Some(d) => {
                let _ = writeln!(s, "tree dirty {d}");
            }
        }
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
        let tree = get.get("tree")?;
        Some(Self {
            head: get.get("head")?.clone(),
            dirty: tree.strip_prefix("dirty ").map(str::to_string),
            mode: get.get("mode")?.clone(),
            scope: get.get("scope")?.clone(),
            verdict: get.get("verdict")?.clone(),
            merge_contract: get.get("merge-contract").is_some_and(|v| v == "yes"),
            skipped: get.get("skipped").cloned().unwrap_or_default(),
            when: get.get("when").and_then(|w| w.parse().ok()).unwrap_or(0),
        })
    }

    /// `Ok(())` when this receipt says the commit may be pushed, else the
    /// sentence saying which half is missing.
    ///
    /// Both halves are required and neither implies the other: a PASS on a
    /// DIRTY tree verified bytes nobody is pushing, and a clean tree with a
    /// narrowed or skipping run never discharged the contract.
    ///
    /// # Errors
    /// The reason, in the words a hook should print.
    pub fn admits_push(&self) -> Result<(), String> {
        if let Some(d) = &self.dirty {
            return Err(format!(
                "the gate's {} was over {} PLUS uncommitted work (dirty {d}) — those are not \
                 the bytes this push sends",
                self.verdict, self.head
            ));
        }
        if !self.merge_contract {
            let skipped = if self.skipped.is_empty() || self.skipped == "none" {
                String::new()
            } else {
                format!("; skipped: {}", self.skipped)
            };
            return Err(format!(
                "the gate ran on {} and did NOT discharge the merge contract (verdict {}, \
                 mode {}, scope {}{skipped})",
                self.head, self.verdict, self.mode, self.scope
            ));
        }
        Ok(())
    }
}

/// `<root>/.aterm-verify/receipts`.
#[must_use]
pub fn dir(root: &Path) -> PathBuf {
    root.join(crate::identity::GATE_STATE_DIR).join(RECEIPT_DIR)
}

/// Write `r` under `root`, keyed by the commit it is about.
///
/// Best effort in one direction only: a receipt that cannot be written costs
/// the NEXT push a refusal, never this run its verdict. The gate says so on
/// stderr rather than failing, because a read-only state directory is an
/// operator's problem with their disk and not a finding about their change.
pub fn write(root: &Path, r: &Receipt) -> std::io::Result<PathBuf> {
    let dir = dir(root);
    std::fs::create_dir_all(&dir)?;
    prune(&dir);
    let path = dir.join(&r.head);
    // Whole-file replace: a half-written receipt that still parsed would be the
    // one failure mode that matters here.
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, r.render())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
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
            dirty: None,
            mode: "fast".into(),
            scope: "workspace".into(),
            verdict: "PASS".into(),
            merge_contract: true,
            skipped: "none".into(),
            when: 1_758_000_000,
        }
    }

    #[test]
    fn a_receipt_round_trips_and_a_green_one_admits_the_push() {
        let r = green();
        assert_eq!(Receipt::parse(&r.render()), Some(r.clone()));
        assert_eq!(r.admits_push(), Ok(()));
    }

    #[test]
    fn a_pass_over_a_dirty_tree_never_admits_a_push() {
        let mut r = green();
        r.dirty = Some("deadbeef1234".into());
        let why = r
            .admits_push()
            .expect_err("a dirty PASS is not about the pushed bytes");
        assert!(why.contains("uncommitted work"), "{why}");
    }

    #[test]
    fn a_narrowed_or_failed_run_never_admits_a_push() {
        for (verdict, contract, scope) in [
            ("FAIL", false, "workspace"),
            ("COULD-NOT-RUN", false, "workspace"),
            ("PASS", false, "aterm-gui"),
        ] {
            let mut r = green();
            r.verdict = verdict.into();
            r.merge_contract = contract;
            r.scope = scope.into();
            assert!(r.admits_push().is_err(), "{verdict} {scope}");
        }
    }

    /// AN UNREADABLE RECEIPT IS NO RECEIPT. The one direction this must never
    /// fail in: a file the hook cannot understand must refuse the push, never
    /// wave it through.
    #[test]
    fn anything_that_is_not_this_format_parses_as_nothing() {
        for text in [
            "",
            "aterm-verify receipt 2\nhead x\ntree clean\n",
            "head x\ntree clean\nmode fast\nscope workspace\nverdict PASS\n",
            &format!("{MAGIC}\nhead x\n"),
        ] {
            assert_eq!(Receipt::parse(text), None, "{text:?}");
        }
    }
}
