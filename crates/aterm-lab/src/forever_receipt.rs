// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING'S RECEIPT, JUDGED (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9, slice
//! 9b). A landing chain's every land `DONE` note is followed, as the journal's next line, by a
//! `RECEIPT` of the same seq whose head is the chain's repository's `HEAD` (its first twelve
//! digits, read with `git rev-parse HEAD` once the watch is over) on `origin/main`: the chains'
//! repositories are pushed to a bare origin ([`super::lay_repo_at`]), so the want is exact. Slice
//! 9c's land-n (a repository with no remote) wants that head on no ref, `leg=no-remote`, and
//! land-o (a plain folder) no head, `leg=no-repo` ([`super::checked::leg_of`]). Each landing
//! stands on the land check's `LANDED` (§14.17 item 5, slice 13a), so its receipt says the check
//! was answered (`check=land-check@v1`) and quotes the defense the check audited, never the
//! word. A chain whose Land holds journals no `RECEIPT` at all.

use std::path::Path;

use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg};
use aterm_agent::harness::land_receipt::{HEAD_DIGITS, RECEIPT};
use aterm_agent::supervise::policy::land_check::{RULE_LAND_CHECK, landed_reply};
use aterm_agent::supervise::policy::turn_end::RULE_LAND;

use super::{Chain, Scenario, Seen, Verdict, at};
use crate::util::one_line;

/// The want's name in a verdict's words.
pub const WANT: &str = "receipt";

/// Whether scenario `s` of `chain` carries the receipt's want: `Some(true)` where the chain
/// lands (one scenario of each: through the land instruction, an instant reply, `land-answer`,
/// a `Wrote` reply, a spawned worker, a usage wall, the work Land's contradiction named landed,
/// and the orchestrator's turn after a person's own exchange, whose `LEFT` note the stray check
/// holds to no `RECEIPT`), `Some(false)` where
/// Land does not land (its holds, a need above the window).
#[must_use]
pub const fn judged(chain: Chain, s: Scenario) -> Option<bool> {
    match (chain, s) {
        (Chain::LandA, Scenario::L1)
        | (Chain::LandN, Scenario::L18)
        | (Chain::LandO, Scenario::L19)
        | (Chain::LandE, Scenario::L7)
        | (Chain::LandJ, Scenario::L16)
        | (Chain::LandK, Scenario::L17)
        | (Chain::InstantLand, Scenario::P4L)
        | (Chain::LandQ, Scenario::L12)
        | (Chain::LandW, Scenario::L14)
        | (Chain::Worker, Scenario::L15)
        | (Chain::LandP, Scenario::L13)
        | (Chain::LandContradicted, Scenario::LX)
        | (Chain::Wall, Scenario::W1) => Some(true),
        (
            Chain::LandB
            | Chain::LandC
            | Chain::LandD
            | Chain::LandF
            | Chain::LandG
            | Chain::LandH
            | Chain::LandI
            | Chain::SameLand,
            _,
        ) => Some(false),
        _ => None,
    }
}

/// Whether the lab reads `chain`'s `HEAD` once its watch is over ([`head_of`]): a chain whose
/// receipt it judges, or one that lands ([`Chain::lands`], whose `status` slice 9c holds to it),
/// in a repository.
#[must_use]
pub fn reads_head(chain: Chain) -> bool {
    super::checked::leg_of(chain) != ReceiptLeg::NoRepo
        && (chain.lands()
            || chain
                .scenarios()
                .iter()
                .any(|&s| judged(chain, s) == Some(true)))
}

/// `verdict` with the receipt's want beside it, where scenario `s` of `chain` carries one
/// ([`judged`]).
#[must_use]
pub fn with_receipt(chain: Chain, s: Scenario, seen: &Seen, t0: i64, verdict: Verdict) -> Verdict {
    let Some(lands) = judged(chain, s) else {
        return verdict;
    };
    let want = if lands {
        landed_as(super::checked::leg_of(chain), seen, t0)
    } else {
        held(seen, t0)
    };
    let why = format!(
        "{}; {WANT}: {} ({})",
        verdict.why(),
        want.word(),
        want.why()
    );
    match (&verdict, &want) {
        (Verdict::Fail(_), _) | (_, Verdict::Fail(_)) => Verdict::Fail(why),
        (Verdict::Unobserved(_), _) | (_, Verdict::Unobserved(_)) => Verdict::Unobserved(why),
        _ => Verdict::Pass(why),
    }
}

/// The seq a journal line names (`seq=<n>`).
fn seq_of(line: &str) -> Option<u64> {
    line.split(' ')
        .find_map(|w| w.strip_prefix("seq="))
        .and_then(|n| n.parse().ok())
}

/// Whether a journal line is a land `DONE` note.
fn land_note(line: &str) -> bool {
    line.starts_with("DONE ") && line.contains(&format!(" rule={RULE_LAND} "))
}

/// Whether a journal line is a receipt's.
fn receipt_line(line: &str) -> bool {
    line.strip_prefix(RECEIPT)
        .is_some_and(|rest| rest.starts_with(' '))
}

/// A LANDING'S RECEIPT: each land `DONE` note after `t0` is followed by a `RECEIPT` of its seq,
/// whose head is the chain's `HEAD`'s first twelve digits, of leg `leg` (slice 9c): on main, the
/// head on `origin/main`; with no remote, the head on no ref; with no repository, no head and no
/// ref. It says the land check was answered and quotes the defense it audited (slice 13a).
#[must_use]
pub fn landed_as(leg: ReceiptLeg, seen: &Seen, t0: i64) -> Verdict {
    let journal: Vec<&(i64, String)> = seen.journal.iter().filter(|(t, _)| *t > t0).collect();
    let notes: Vec<usize> = (0..journal.len())
        .filter(|&i| land_note(&journal[i].1))
        .collect();
    let Some(&first) = notes.first() else {
        return Verdict::Unobserved(format!("no land DONE note to +{}", at(t0, seen.end_ms)));
    };
    let want = if leg == ReceiptLeg::NoRepo {
        ""
    } else {
        let Some(head) = seen.head.as_deref() else {
            return Verdict::Unobserved("the chain's HEAD was not read".into());
        };
        let Some(want) = head.get(..HEAD_DIGITS) else {
            return Verdict::Fail(format!("the chain's HEAD {head:?} is no commit"));
        };
        want
    };
    let on = if leg == ReceiptLeg::OnMain {
        "origin/main"
    } else {
        ""
    };
    let mut bad = Vec::new();
    let mut read: Option<LandReceipt> = None;
    for &i in &notes {
        let (t, note) = journal[i];
        let seq = seq_of(note);
        let next = journal.get(i + 1).map(|(_, l)| l.as_str());
        let Some(next) = next.filter(|l| receipt_line(l)) else {
            bad.push(format!(
                "the DONE note at +{} (seq={}) is not followed by a RECEIPT: next {:?}",
                at(t0, *t),
                seq.map_or_else(|| "?".into(), |s| s.to_string()),
                next.map(|l| one_line(l, 100))
            ));
            continue;
        };
        match LandReceipt::of_line(next, *t) {
            None => bad.push(format!(
                "a RECEIPT line no build writes: {}",
                one_line(next, 160)
            )),
            Some((at_seq, _)) if Some(at_seq) != seq => bad.push(format!(
                "the RECEIPT names seq={at_seq}, its DONE note seq={}: {}",
                seq.map_or_else(|| "?".into(), |s| s.to_string()),
                one_line(next, 160)
            )),
            Some((_, r)) => {
                if r.head != want || r.leg != leg || r.main_ref != on {
                    bad.push(format!(
                        "the RECEIPT says head={} on={} leg={}, wanted head={want} on={on} \
                         leg={}: {}",
                        r.head,
                        r.main_ref,
                        r.leg.word(),
                        leg.word(),
                        one_line(next, 160)
                    ));
                }
                if !r.check {
                    bad.push(format!(
                        "the RECEIPT says no land check was answered (no \
                         check={RULE_LAND_CHECK}): {}",
                        one_line(next, 160)
                    ));
                } else if r.said.is_empty() || landed_reply(Some(&r.said)) {
                    bad.push(format!(
                        "the RECEIPT quotes {:?}, not the defense the check audited: {}",
                        r.said,
                        one_line(next, 160)
                    ));
                }
                read.get_or_insert(r);
            }
        }
    }
    let strays = journal
        .iter()
        .enumerate()
        .filter(|(i, (_, l))| receipt_line(l) && (*i == 0 || !notes.contains(&(i - 1))))
        .count();
    if strays > 0 {
        bad.push(format!("{strays} RECEIPT line(s) under no DONE note"));
    }
    let said = format!(
        "{} DONE note(s) from +{}, each followed by its RECEIPT{}",
        notes.len(),
        at(t0, journal[first].0),
        read.map_or_else(String::new, |r| format!(
            " (head={} on={} leg={} check={} said {:?})",
            r.head,
            r.main_ref,
            r.leg.word(),
            if r.check { RULE_LAND_CHECK } else { "-" },
            one_line(&r.said, 60)
        ))
    );
    if bad.is_empty() {
        Verdict::Pass(format!(
            "{said}, HEAD {}",
            if want.is_empty() { "-" } else { want }
        ))
    } else {
        Verdict::Fail(bad.join("; "))
    }
}

/// A HOLD HAS NO RECEIPT: no `RECEIPT` line after `t0`.
#[must_use]
pub fn held(seen: &Seen, t0: i64) -> Verdict {
    match seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && receipt_line(l))
    {
        Some((t, l)) => Verdict::Fail(format!(
            "a RECEIPT on a hold at +{}: {}",
            at(t0, *t),
            one_line(l, 160)
        )),
        None => Verdict::Pass(format!("no RECEIPT to +{}", at(t0, seen.end_ms))),
    }
}

/// The chain's repository's `HEAD` in full, read in `dir` with the lab's `HOME`, or `None`.
#[must_use]
pub fn head_of(home: &Path, dir: &Path) -> Option<String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .ok()?;
    let head = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && head.len() >= HEAD_DIGITS).then_some(head)
}

#[cfg(test)]
#[path = "forever_receipt_tests.rs"]
mod tests;
