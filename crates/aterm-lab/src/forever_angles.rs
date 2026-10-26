// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cruise-angles` (`--mode angles`, v6 §14.5's 5a): Cruise asks for a new kind of work at each
//! done claim. Every reply of its conversation is real work past the short-turn floor
//! (`turn_end::MIN_WORK`) that ends in a done claim, each one its own, so neither hold meets it;
//! the done check at each claim asks the next of `turn_end::DONE_ANGLES` (a seventh, where one is
//! typed, the first again), and nothing holds the tab past the sixth. Each done claim is a short
//! turn whatever work it reports, so the back-off before each check doubles; the run's aterm
//! takes `--mode ladder`'s seam ([`super::ladder_env`]), the ladder at an eighth, or the sixth
//! check would come past +79 min.

use std::path::{Path, PathBuf};

use aterm_agent::supervise::policy::turn_end::{self, DONE_ANGLES, RULE_DONE_CHECK};

use super::surfaces::held::{decided_after, stall_of};
use super::{PAST_NEXT_STEP_MS, Seen, Verdict, at, git, git_out, lost_after, streak_at};

/// How many done checks the chain needs: each angle once.
pub const CHECKS: usize = DONE_ANGLES.len();

/// How many it judges: a seventh, where the watch saw one, is the first again.
const JUDGED: usize = CHECKS + 1;

/// The reply to the first prompt: real work, then a done claim.
pub const FIRST: &str = "Implemented the parser module for this repository together with its \
     unit tests and a small fuzz harness, checked every suite on the branch, merged the branch \
     into main and pushed it to origin/main, where the whole parser now lives with its tests \
     beside it, and the readme now says how to run them and what each one covers. All done.";

/// What the stub reads in each angle's check ([`DONE_ANGLES`], by index): words of that angle
/// alone, so a check that asks another kind of work (v1's one fixed check among them) gets
/// [`FIRST`] again.
pub const KEYS: [&str; DONE_ANGLES.len()] = [
    "run the widest check",
    "as a hostile reviewer would",
    "called out of scope",
    "against the main it started from",
    "the next open issue or TODO",
    "land every worktree, branch and stash you made",
];

/// The reply to each angle's check (by index): that kind of work done, then a done claim, each
/// in words of its own (no digits: `reply_mark` reads none), opening with none of Muse Code's
/// tool-row heads, no `Nothing left:` and no need.
pub const REPLIES: [&str; DONE_ANGLES.len()] = [
    "The widest check this repository has ran on the merged tree, the whole suite with the fuzz \
     harness and the linter, and it found one flaky parser test that raced on a temporary file; \
     that test now uses its own directory, and the fix is merged and pushed to origin/main, \
     where the whole suite passed again twice in a row. All done.",
    "Reviewed the landed diff as a hostile reviewer would and found that empty input and a lone \
     quote both read past the end of the tokenizer's buffer; each path now returns a parse error \
     with a test of its own, and the hardened tokenizer is merged and pushed to origin/main. All \
     done.",
    "Took up what the earlier reports had set aside: the grammar reference for the parser, its \
     error messages with the line and column, and a usage section in the readme, all reviewed \
     against the code, merged into main and pushed to origin/main, where the documentation build \
     renders every page of it without a warning. All done.",
    "Measured the parser against the main it started from on the benchmark corpus: parsing is no \
     slower anywhere and the large-file case is faster, while memory stays flat; the timing \
     harness and its baseline are committed, merged and pushed to origin/main, so the measurement \
     can be taken again the same way. All done.",
    "Took the open issue beside the parser about unicode identifiers, taught the tokenizer to \
     accept them under the standard's rules, added tests for the accepted and the refused forms, \
     and merged the change into main and pushed it to origin/main, where the issue now names the \
     commit that closed it. All done.",
    "Landed the one branch this session made, merged into main and pushed to origin/main, and \
     checked that no worktree or stash of this session is left; every worktree, branch and stash \
     this session did not make stands exactly as it was, untouched, with nothing of it read or \
     moved. All done.",
];

/// The other session's branch ([`lay_others`]), its one commit's subject, and its file.
pub const OTHER_BRANCH: &str = "other-session";
const OTHER_SUBJECT: &str = "another session: its parser draft";
/// The other session's worktree's branch; the worktree stands beside the chain's folder, named
/// [`other_worktree`].
pub const OTHER_WORKTREE_BRANCH: &str = "other-wt";
/// The other session's stash entry's message.
pub const OTHER_STASH: &str = "another session: its readme draft";

/// Where the other session's worktree stands: beside `dir`, `<dir>-other`.
#[must_use]
pub fn other_worktree(dir: &Path) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push("-other");
    dir.with_file_name(name)
}

/// [`super::Repo::Others`] laid in `dir`, whose `main` is committed and pushed: a branch
/// ([`OTHER_BRANCH`]) holding a commit no remote has, a worktree on a branch of its own
/// ([`OTHER_WORKTREE_BRANCH`], [`other_worktree`]), and a stash entry ([`OTHER_STASH`]), `main`
/// left clean and checked out.
/// # Errors
/// A git step that fails.
pub fn lay_others(home: &Path, dir: &Path) -> Result<(), String> {
    let g = |args: &[&str]| git(home, dir, args);
    g(&["switch", "-q", "-c", OTHER_BRANCH])?;
    std::fs::write(dir.join("other.txt"), "another session's parser draft\n")
        .map_err(|e| format!("other.txt: {e}"))?;
    g(&["add", "other.txt"])?;
    g(&["commit", "-q", "-m", OTHER_SUBJECT])?;
    g(&["switch", "-q", "main"])?;
    let wt = other_worktree(dir);
    g(&[
        "worktree",
        "add",
        "-q",
        "-b",
        OTHER_WORKTREE_BRANCH,
        &wt.to_string_lossy(),
    ])?;
    std::fs::write(
        dir.join("README.md"),
        "# The forever lab\n\nAnother session's draft.\n",
    )
    .map_err(|e| format!("README.md: {e}"))?;
    g(&["stash", "push", "-q", "-m", OTHER_STASH])
}

/// What of [`lay_others`]'s work no longer stands in `dir`: the worktree (listed, on its branch),
/// the branch (its commit's subject), the stash entry (its message), each named where it is gone
/// or moved, or the git read that failed. Empty: all of it stands.
#[must_use]
pub fn others_left(home: &Path, dir: &Path) -> Vec<String> {
    let mut gone = Vec::new();
    let wt = std::fs::canonicalize(other_worktree(dir)).unwrap_or_else(|_| other_worktree(dir));
    match git_out(home, dir, &["worktree", "list", "--porcelain"]) {
        Ok(list) => {
            let stands = list.split("\n\n").any(|entry| {
                entry.lines().any(|l| {
                    l.strip_prefix("worktree ")
                        .is_some_and(|p| std::fs::canonicalize(p).is_ok_and(|p| p == wt))
                }) && entry
                    .lines()
                    .any(|l| l == format!("branch refs/heads/{OTHER_WORKTREE_BRANCH}"))
            });
            if !stands {
                gone.push(format!(
                    "the worktree {} on {OTHER_WORKTREE_BRANCH}",
                    wt.display()
                ));
            }
        }
        Err(e) => gone.push(e),
    }
    match git_out(home, dir, &["log", "-1", "--format=%s", OTHER_BRANCH, "--"]) {
        Ok(subject) if subject.trim() == OTHER_SUBJECT => {}
        Ok(subject) => gone.push(format!(
            "the branch {OTHER_BRANCH} (its last commit now {:?})",
            subject.trim()
        )),
        Err(e) => gone.push(format!("the branch {OTHER_BRANCH} ({e})")),
    }
    match git_out(home, dir, &["stash", "list", "--format=%s"]) {
        Ok(list) if list.lines().any(|l| l.ends_with(OTHER_STASH)) => {}
        Ok(_) => gone.push(format!("the stash entry {OTHER_STASH:?}")),
        Err(e) => gone.push(e),
    }
    gone
}

/// How long the chain may take to watch past its sixth check: seven replies past the floor, the
/// back-offs before checks two to six and the one after the sixth at an eighth (15 s doubled to
/// 7.5 min), and the reads between them, about 29 min on Claude Code.
pub const LIMIT_MS: i64 = 40 * 60 * 1000;

/// The chain's done checks, in order: when each was journaled and what it typed (the ledger's
/// whole text where it has the row, the journal's head otherwise).
fn checks(seen: &Seen) -> Vec<(i64, &str)> {
    seen.acts(RULE_DONE_CHECK)
        .into_iter()
        .map(|a| (a.0, seen.act_text(RULE_DONE_CHECK, a)))
        .collect()
}

/// Whether check `typed` is the `n`th claim's (counting from 0): it opens with
/// `turn_end::done_check_words`'s words for `n`, whatever suffix follows them.
fn asks(typed: &str, n: usize) -> bool {
    typed.starts_with(&turn_end::done_check_words(n))
}

/// The first check that is not its claim's angle, by index.
fn first_wrong(checks: &[(i64, &str)]) -> Option<usize> {
    checks
        .iter()
        .take(JUDGED)
        .enumerate()
        .find(|(n, (_, typed))| !asks(typed, *n))
        .map(|(n, _)| n)
}

/// When the sixth check's point was decided: the first decision journaled after it (a `WAITING`,
/// the hold, a `DONE` or a `LEFT`), with the sixth's time.
fn sixth_decided<'a>(seen: &'a Seen, checks: &[(i64, &str)]) -> Option<(i64, i64, &'a str)> {
    let sixth = checks.get(CHECKS - 1)?.0;
    decided_after(seen, sixth).map(|(t, l)| (sixth, *t, l.as_str()))
}

/// THE CHAIN'S WATCH ([`super::finished`]): its sixth check's point decided and
/// [`PAST_NEXT_STEP_MS`] watched past that, or a check that asks the wrong angle, a hold or a
/// `DONE` journaled 5 s ago (the run is decided at it).
#[must_use]
pub fn watched(seen: &Seen, now: i64, t0: i64) -> bool {
    let checks = checks(seen);
    let decided_at = first_wrong(&checks)
        .map(|n| checks[n].0)
        .into_iter()
        .chain(stall_of(seen, t0).map(|(t, _)| *t))
        .chain(seen.first_after("DONE ", t0).map(|(t, _)| *t))
        .min();
    if decided_at.is_some_and(|t| now - t >= 5_000) {
        return true;
    }
    sixth_decided(seen, &checks).is_some_and(|(_, decided, _)| now - decided >= PAST_NEXT_STEP_MS)
}

/// The angle a check's text asks, for the verdict: its index, or `?`.
fn angle_of(typed: &str) -> String {
    (0..DONE_ANGLES.len())
        .find(|&n| asks(typed, n))
        .map_or_else(|| "?".to_string(), |n| (n + 1).to_string())
}

/// ANGLES (§14.5's Proof, live): the first [`CHECKS`] done checks ask the six angles in order, six
/// texts no two alike, and a seventh, where one was typed, the first again; no hold
/// (`kind=stalled`) from the first prompt to [`PAST_NEXT_STEP_MS`] past the sixth check's point's
/// decision, and never `DONE` (Cruise never ends).
#[must_use]
pub fn judge(seen: &Seen, t0: i64) -> Verdict {
    let checks = checks(seen);
    let said = format!(
        "checks [{}]",
        checks
            .iter()
            .take(JUDGED)
            .map(|(t, typed)| format!(
                "angle {} at +{} (short turns {})",
                angle_of(typed),
                at(t0, *t),
                streak_at(seen, *t).map_or_else(|| "?".to_string(), |n| n.to_string())
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(n) = first_wrong(&checks) {
        let (t, typed) = checks[n];
        let again = checks[..n]
            .iter()
            .position(|(_, before)| *before == typed)
            .map_or_else(String::new, |k| {
                format!(", word for word check {}'s", k + 1)
            });
        return Verdict::Fail(format!(
            "{said}: CHECK {} AT +{} DOES NOT ASK ANGLE {}{again} ({})",
            n + 1,
            at(t0, t),
            n % DONE_ANGLES.len() + 1,
            crate::util::one_line(typed, 110)
        ));
    }
    if let Some((t, l)) = stall_of(seen, t0) {
        return Verdict::Fail(format!(
            "{said}: HELD at +{} ({}), though every reply is its own work",
            at(t0, *t),
            crate::util::one_line(l, 120)
        ));
    }
    if let Some((t, l)) = seen.first_after("DONE ", t0) {
        return Verdict::Fail(format!(
            "{said}: DONE at +{} under Cruise ({})",
            at(t0, *t),
            crate::util::one_line(l, 100)
        ));
    }
    if checks.len() < CHECKS {
        let since = checks.last().map_or(t0, |(t, _)| *t);
        return match lost_after(seen, since, t0) {
            Some(lost) => Verdict::Fail(format!("{said}: {lost}")),
            None => Verdict::Unobserved(format!(
                "{said}: {} of {CHECKS} checks within +{}",
                checks.len(),
                at(t0, seen.end_ms)
            )),
        };
    }
    let Some((sixth, decided, how)) = sixth_decided(seen, &checks) else {
        return Verdict::Unobserved(format!(
            "{said}: the sixth check's point not decided by +{}",
            at(t0, seen.end_ms)
        ));
    };
    if seen.end_ms - decided < PAST_NEXT_STEP_MS {
        return Verdict::Unobserved(format!(
            "{said}: watched {} past the sixth check's point, under {} s",
            at(0, seen.end_ms - decided),
            PAST_NEXT_STEP_MS / 1000
        ));
    }
    let others = match &seen.cover.others_left {
        Some(gone) if !gone.is_empty() => {
            return Verdict::Fail(format!(
                "{said}: ANOTHER SESSION'S WORK TOUCHED: {}",
                gone.join("; ")
            ));
        }
        Some(_) => "; the other session's worktree, branch and stash stand as laid",
        None => {
            return Verdict::Unobserved(format!(
                "{said}: the other session's work NOT READ, so its want (left exactly as laid) is \
                 unjudged"
            ));
        }
    };
    let again = if checks.len() > CHECKS {
        ", then the first again"
    } else {
        ""
    };
    Verdict::Pass(format!(
        "{said}: six kinds of work asked in turn{again}, no hold to +{} (the sixth's point decided \
         at +{}, {}){others}",
        at(t0, seen.end_ms),
        at(t0, decided.max(sixth)),
        crate::util::one_line(how, 90)
    ))
}

#[cfg(test)]
#[path = "forever_angles_tests.rs"]
mod tests;
