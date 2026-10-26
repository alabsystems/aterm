// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `cruise-angles` (v6 §14.5's 5a): its script and its judge, over records shaped as
//! the loop journals them.

use aterm_agent::supervise::policy::needs::{defense_line, needs_you};
use aterm_agent::supervise::policy::turn_end::{
    DONE_CHECK_FIXED_BEFORE, MIN_WORK, done_check_act, done_check_words, open_work, says_done,
};

use std::path::PathBuf;

use super::*;
use crate::forever::{Chain, Row, Seen, git, git_out, long_reply_ms, spec};
use crate::pyjson::J;

/// The first prompt (unix ms).
const T0: i64 = 1_792_900_000_000;

/// The floor every reply's turn clears, with room for the reads around it.
fn floor_ms() -> i64 {
    i64::try_from(MIN_WORK.as_millis()).unwrap_or(i64::MAX) + 20_000
}

/// The heads Muse Code's tool rows open with (`aterm_phase::muse`'s, as its captures measured
/// them): a reply opening with one is read as a tool's row, not the agent's words.
const MUSE_HEADS: [&str; 7] = [
    "Ran ",
    "Wrote ",
    "Edited ",
    "Read memory ",
    "Saved memory ",
    "Read goal ",
    "Used tool ",
];

/// Every reply of the chain is real work past the floor that ends in a done claim of its own: no
/// digit, no defense, no need, no open work, no tool-row head, no two alike. NEGATIVE CONTROLS:
/// the measured pass's done claim is under the floor even paced long, its work report claims
/// nothing done, and the first draft's hostile review named open work ("unchecked").
#[test]
fn every_reply_is_real_work_and_a_done_claim_of_its_own() {
    let all: Vec<&str> = std::iter::once(FIRST).chain(REPLIES).collect();
    for (i, r) in all.iter().enumerate() {
        assert!(says_done(Some(r)), "{r}");
        assert!(
            long_reply_ms(r) >= floor_ms(),
            "{} ms: {r}",
            long_reply_ms(r)
        );
        assert!(
            long_reply_ms(r) <= 4 * 60_000,
            "{} ms: {r}",
            long_reply_ms(r)
        );
        assert!(!r.chars().any(|c| c.is_ascii_digit()), "{r}");
        assert!(defense_line(r).is_none(), "{r}");
        assert!(needs_you(r).is_empty(), "{r}");
        assert_eq!(open_work(Some(r)), Vec::<&str>::new(), "{r}");
        assert!(!MUSE_HEADS.iter().any(|h| r.starts_with(h)), "{r}");
        assert!(!r.contains('\n'), "{r}");
        assert!(all[..i].iter().all(|before| before != r), "{i} again: {r}");
    }
    assert!(long_reply_ms(super::super::DONE_CLAIM) < floor_ms());
    let draft = "Reviewed the landed diff and found that a lone quote reached an unchecked index \
                 in the tokenizer; it now returns a parse error. All done.";
    assert_eq!(open_work(Some(draft)), ["unchecked"]);
    assert!(!says_done(Some(super::super::WORK)));
}

fn users(acts: &[&str]) -> Vec<String> {
    std::iter::once(Chain::CruiseAngles.prompt())
        .chain(acts.iter().map(|s| (*s).to_string()))
        .collect()
}

fn scripted(acts: &[&str]) -> J {
    crate::stub::scripted_reply(&spec(&Chain::every()), &users(acts))
        .expect("a rule")
        .clone()
}

fn text(rule: &J) -> &str {
    rule.get("text").and_then(J::as_str).unwrap_or_default()
}

/// Each angle's check, bare or with the rules' and the repository's suffixes, gets that angle's
/// reply, paced long; the first prompt, and any check that asks no angle, the first prompt's.
/// NEGATIVE CONTROL: v1's one fixed check gets the first prompt's reply, whichever claim it is.
#[test]
fn each_angles_check_gets_its_own_reply_and_any_other_the_first_prompts() {
    assert_eq!(text(&scripted(&[])), FIRST);
    for n in 0..DONE_ANGLES.len() {
        for check in [
            done_check_words(n),
            done_check_act(n, Some("never push to main"), true),
        ] {
            let before: Vec<String> = (0..n).map(done_check_words).collect();
            let mut acts: Vec<&str> = before.iter().map(String::as_str).collect();
            acts.push(&check);
            let rule = scripted(&acts);
            assert_eq!(text(&rule), REPLIES[n], "angle {}", n + 1);
            assert!(
                rule.get("delay_ms").and_then(J::as_i64).unwrap_or(0) > 0,
                "{rule:?}"
            );
        }
        for key in KEYS.iter().enumerate().filter(|(k, _)| *k != n) {
            assert!(!DONE_ANGLES[n].contains(key.1), "{} in {n}", key.0);
        }
    }
    assert_eq!(text(&scripted(&[DONE_CHECK_FIXED_BEFORE])), FIRST);
    assert_eq!(
        text(&scripted(&[
            DONE_CHECK_FIXED_BEFORE,
            DONE_CHECK_FIXED_BEFORE
        ])),
        FIRST
    );
}

/// A journal line at `t` ms after [`T0`].
fn j(t: i64, line: &str) -> (i64, String) {
    (T0 + t, line.to_string())
}

/// The records of `typed` done checks, the `n`th journaled at `(n + 1) * 150 s` with the point
/// before it and its own point decided 140 s after it (the back-off's `WAITING`), and the watch
/// ending `end` ms after [`T0`].
fn records(typed: &[String], end: i64) -> Seen {
    let mut seen = Seen {
        end_ms: T0 + end,
        ..Seen::default()
    };
    // The run read the other session's work at its end, and all of it stood.
    seen.cover.others_left = Some(Vec::new());
    for (n, check) in typed.iter().enumerate() {
        let t = i64::try_from(n + 1).unwrap_or(0) * 150_000;
        let seq = 100 + n;
        seen.journal.push(j(
            t - 1_000,
            &format!("EVENT idle seq={seq} ⏺ Merged into main and pushed it. All done."),
        ));
        let head: String = check.chars().take(200).collect();
        seen.journal.push(j(
            t,
            &format!("CONTINUED seq={seq} rule={RULE_DONE_CHECK} {head}"),
        ));
        seen.ledger.push(Row {
            ts: T0 + t,
            rule: RULE_DONE_CHECK.into(),
            decision: "typed".into(),
            command: check.clone(),
            reason: "the turn-end policy (done checks this Cruise run: 1)".into(),
        });
        seen.journal.push(j(
            t + 140_000,
            &format!(
                "WAITING seq={} until=- {} short turn(s) in a row: 15 s back-off",
                seq + 1,
                n + 1
            ),
        ));
    }
    seen
}

/// `seen` without the decision of its last check's point.
fn undecided(mut seen: Seen) -> Seen {
    let last = seen
        .journal
        .iter()
        .rposition(|(_, l)| l.starts_with("WAITING "));
    seen.journal.remove(last.expect("a WAITING"));
    seen
}

fn angled(n: usize) -> Vec<String> {
    (0..n).map(done_check_words).collect()
}

/// The sixth check is journaled at +900 s, and its point decided at +1040 s.
const SIXTH: i64 = 900_000;
const SIXTH_DECIDED: i64 = SIXTH + 140_000;

/// ANGLES passes on six angles in turn, watched past the sixth check's point with no hold, and a
/// seventh, where one was typed, the first again. NEGATIVE CONTROLS: v1's fixed check at every
/// claim, a hold past the sixth, a seventh that is not the first again, a DONE, the sixth's point
/// undecided, too short a watch past it, too few checks, another session's work touched, and that
/// work never read.
#[test]
fn the_angles_pass_in_turn_and_every_other_record_fails_or_waits() {
    let end = SIXTH_DECIDED + PAST_NEXT_STEP_MS;
    let pass = records(&angled(CHECKS + 1), end);
    let v = judge(&pass, T0);
    assert!(
        matches!(&v, Verdict::Pass(w) if w.contains("then the first again")),
        "{v:?}"
    );
    assert!(v.why().contains("angle 6 at +"), "{v:?}");
    let six = records(&angled(CHECKS), end);
    let v = judge(&six, T0);
    assert!(
        matches!(&v, Verdict::Pass(w) if !w.contains("first again")),
        "{v:?}"
    );
    assert!(v.why().contains("decided at +1040.0 s, WAITING"), "{v:?}");
    let fixed = records(&vec![DONE_CHECK_FIXED_BEFORE.to_string(); CHECKS + 1], end);
    let v = judge(&fixed, T0);
    assert!(
        matches!(&v, Verdict::Fail(w) if w.contains("CHECK 1 AT +") && w.contains("DOES NOT ASK ANGLE 1")),
        "{v:?}"
    );
    let mut held = pass.clone();
    held.journal.push(j(
        SIXTH + 30_000,
        "EXCEPTION seq=900 kind=stalled when=later Its program answers aterm's acts with the same \
         reply and no work, so aterm types nothing more.",
    ));
    let v = judge(&held, T0);
    assert!(
        matches!(&v, Verdict::Fail(w) if w.contains("HELD at +")),
        "{v:?}"
    );
    let mut wrong = angled(DONE_ANGLES.len());
    wrong.push(done_check_words(1));
    let v = judge(&records(&wrong, end), T0);
    assert!(
        matches!(&v, Verdict::Fail(w) if w.contains("CHECK 7 AT +") && w.contains("ANGLE 1, word for word check 2's")),
        "{v:?}"
    );
    let mut done = pass.clone();
    done.journal.push(j(
        SIXTH + 10_000,
        "DONE seq=901 rule=continue-done-check@v1 x",
    ));
    assert!(matches!(judge(&done, T0), Verdict::Fail(_)));
    let short = records(&angled(CHECKS), SIXTH_DECIDED + PAST_NEXT_STEP_MS - 1_000);
    let v = judge(&short, T0);
    assert!(
        matches!(&v, Verdict::Unobserved(w) if w.contains("under 60 s")),
        "{v:?}"
    );
    let v = judge(&undecided(records(&angled(CHECKS), end)), T0);
    assert!(
        matches!(&v, Verdict::Unobserved(w) if w.contains("point not decided")),
        "{v:?}"
    );
    let mut stood = pass.clone();
    stood.cover.others_left = Some(Vec::new());
    let v = judge(&stood, T0);
    assert!(
        matches!(&v, Verdict::Pass(w) if w.ends_with("stand as laid")),
        "{v:?}"
    );
    let mut unread = pass.clone();
    unread.cover.others_left = None;
    let v = judge(&unread, T0);
    assert!(
        matches!(&v, Verdict::Unobserved(w) if w.contains("the other session's work NOT READ")),
        "{v:?}"
    );
    let mut touched = pass.clone();
    touched.cover.others_left = Some(vec![format!("the stash entry {OTHER_STASH:?}")]);
    let v = judge(&touched, T0);
    assert!(
        matches!(&v, Verdict::Fail(w) if w.contains("ANOTHER SESSION'S WORK TOUCHED")),
        "{v:?}"
    );
    let few = records(&angled(4), 4 * 150_000 + 30_000);
    let v = judge(&few, T0);
    assert!(
        matches!(&v, Verdict::Unobserved(w) if w.contains("4 of 6 checks")),
        "{v:?}"
    );
}

/// The watch ends [`PAST_NEXT_STEP_MS`] past the sixth check's point's decision, or 5 s past what
/// decides the run. NEGATIVE CONTROLS: before either it goes on, and with the sixth's point
/// undecided it waits for it.
#[test]
fn the_watch_ends_past_the_sixth_or_at_what_decides_the_run() {
    for typed in [CHECKS, CHECKS + 1] {
        let seen = records(&angled(typed), 0);
        assert!(
            !watched(&seen, T0 + SIXTH_DECIDED + PAST_NEXT_STEP_MS - 1, T0),
            "{typed}"
        );
        assert!(
            watched(&seen, T0 + SIXTH_DECIDED + PAST_NEXT_STEP_MS, T0),
            "{typed}"
        );
    }
    let open = undecided(records(&angled(CHECKS), 0));
    assert!(!watched(&open, T0 + 2 * SIXTH_DECIDED, T0));
    let fixed = records(&vec![DONE_CHECK_FIXED_BEFORE.to_string(); 2], 0);
    assert!(!watched(&fixed, T0 + 150_000 + 4_000, T0));
    assert!(watched(&fixed, T0 + 150_000 + 5_000, T0));
    let mut held = records(&angled(3), 0);
    held.journal
        .push(j(460_000, "EXCEPTION seq=9 kind=stalled when=later x"));
    assert!(!watched(&held, T0 + 464_000, T0));
    assert!(watched(&held, T0 + 465_000, T0));
}

/// A scratch folder of its own for `name`, empty.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("forever-angles-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(other_worktree(&d));
    std::fs::create_dir_all(&d).expect("the scratch folder");
    d
}

/// Another session's work laid beside a committed `main` stands as laid, and each piece of it
/// gone is named. NEGATIVE CONTROLS: the stash dropped, the branch deleted, the worktree removed,
/// each alone.
#[test]
fn another_sessions_work_stands_as_laid_and_each_piece_gone_is_named() {
    for gone in ["", "stash", "branch", "worktree"] {
        let dir = scratch(if gone.is_empty() { "laid" } else { gone });
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let g = |args: &[&str]| git(&home, &dir, args).expect("git");
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README.md"), "# lab\n").expect("readme");
        g(&["add", "README.md"]);
        g(&["commit", "-q", "-m", "lab: the scratch project"]);
        lay_others(&home, &dir).expect("laid");
        let status = git_out(&home, &dir, &["status", "--porcelain"]).expect("status");
        assert_eq!(status, "", "main is left clean");
        match gone {
            "stash" => g(&["stash", "drop", "-q"]),
            "branch" => g(&["branch", "-q", "-D", OTHER_BRANCH]),
            "worktree" => g(&[
                "worktree",
                "remove",
                "--force",
                &other_worktree(&dir).to_string_lossy(),
            ]),
            _ => {}
        }
        let left = others_left(&home, &dir);
        let _ = std::fs::remove_dir_all(other_worktree(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        if gone.is_empty() {
            assert!(left.is_empty(), "{left:?}");
        } else {
            assert_eq!(left.len(), 1, "{gone}: {left:?}");
            assert!(left[0].contains(gone), "{gone}: {left:?}");
        }
    }
}
