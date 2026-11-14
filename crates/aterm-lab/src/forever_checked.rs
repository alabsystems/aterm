// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT ATERM CHECKED, ON EVERY SURFACE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9,
//! slice 9c), judged. 9b journals a landing's receipt; 9c says it on the window's surfaces and the
//! control socket's. The lab holds them to the receipt the journal holds (or, with none, to `-`):
//!
//! * `status`'s `landed_head=`, `landed_on=`, `landed_leg=` and `landed_repo=` at every read
//!   whose `task=` is `landed`, and `-` at every other ([`status_agrees`]): on every chain whose
//!   receipt the lab judges, and on the chains that land with no receipt want of their own (W2,
//!   W4–W6, W9–W11, O1) and the switch chains, judged through `landed_head=` alone. A build before
//!   9c prints no `landed_head=`, which fails it.
//! * the leg each chain's folder makes ([`leg_of`]): `on-main` over the lab's pushed
//!   repositories, `no-remote` over land-n's (L18), `no-repo` over land-o's plain folder (L19).
//! * L15: the spawned worker's `checked=` on the manager's `await task` row and `result`'s
//!   header ([`worker_checked`]).
//! * L1, L7, L16–L19: the landed chip's title, read in front, says its own receipt's words in
//!   brief ([`title_checked`]).

use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg};
use aterm_agent::harness::land_receipt::{HEAD_DIGITS, RECEIPT};
use aterm_agent::supervise::mode_words;
use aterm_agent::supervise::policy::turn_end;

use super::{Chain, Repo, Scenario, Seen, Verdict, at, pct_decode, receipt, word};
use crate::util::one_line;

/// The want's name in a verdict's words.
pub const WANT: &str = "checked";

/// The four `status` fields that say what aterm checked.
pub const FIELDS: [&str; 4] = ["landed_head", "landed_on", "landed_leg", "landed_repo"];

/// Whether scenario `s` of `chain` carries 9c's wants: every scenario whose receipt the lab
/// judges ([`receipt::judged`]), the chains that land with no receipt want (W2, W4–W6, W9–W11,
/// O1) and the switch chains.
#[must_use]
pub fn judged(chain: Chain, s: Scenario) -> bool {
    receipt::judged(chain, s).is_some()
        || matches!(
            s,
            Scenario::W2
                | Scenario::W4
                | Scenario::W5
                | Scenario::W6
                | Scenario::W9
                | Scenario::W10
                | Scenario::W11
                | Scenario::O1
                | Scenario::SwitchAgent
                | Scenario::SwitchSeq
                | Scenario::CommandDraft
                | Scenario::SwitchFlip
                | Scenario::SwitchOverHeld
                | Scenario::SwitchBusy
        )
}

/// The leg a landing in `chain`'s folder makes: its repository's ([`Chain::repo`]).
#[must_use]
pub const fn leg_of(chain: Chain) -> ReceiptLeg {
    match chain.repo() {
        Repo::NoRemote => ReceiptLeg::NoRemote,
        Repo::Plain => ReceiptLeg::NoRepo,
        _ => ReceiptLeg::OnMain,
    }
}

/// `verdict` with 9c's wants beside it, where scenario `s` of `chain` carries them ([`judged`]).
#[must_use]
pub fn with_checked(chain: Chain, s: Scenario, seen: &Seen, t0: i64, verdict: Verdict) -> Verdict {
    if !judged(chain, s) {
        return verdict;
    }
    let mut wants = vec![status_agrees(chain, seen, t0)];
    if s == Scenario::L15 {
        wants.push(worker_checked(seen, t0));
    }
    if titled(s) {
        wants.push(title_checked(seen, t0));
    }
    let mut why = verdict.why().to_owned();
    let mut fail = matches!(verdict, Verdict::Fail(_));
    let mut unseen = matches!(verdict, Verdict::Unobserved(_));
    for want in &wants {
        why.push_str(&format!("; {WANT}: {} ({})", want.word(), want.why()));
        fail |= matches!(want, Verdict::Fail(_));
        unseen |= matches!(want, Verdict::Unobserved(_));
    }
    if fail {
        Verdict::Fail(why)
    } else if unseen {
        Verdict::Unobserved(why)
    } else {
        Verdict::Pass(why)
    }
}

/// The landing's receipt the journal holds after `t0`: the last `RECEIPT` line, read.
#[must_use]
pub fn receipt_of(seen: &Seen, t0: i64) -> Option<LandReceipt> {
    seen.journal
        .iter()
        .filter(|(t, l)| *t > t0 && l.starts_with(&format!("{RECEIPT} ")))
        .filter_map(|(t, l)| LandReceipt::of_line(l, *t).map(|(_, r)| r))
        .next_back()
}

/// The receipt a `status` read at `t` speaks for: the last journaled after `t0` and by `t`, or,
/// where the read came before the journal's line (the loop sets the receipt before it writes the
/// line), the first after `t0`. A chain that lands again after a person's turn reads each
/// landing's own.
fn receipt_at(seen: &Seen, t0: i64, t: i64) -> Option<LandReceipt> {
    let all: Vec<(i64, LandReceipt)> = seen
        .journal
        .iter()
        .filter(|(at, l)| *at > t0 && l.starts_with(&format!("{RECEIPT} ")))
        .filter_map(|(at, l)| LandReceipt::of_line(l, *at).map(|(_, r)| (*at, r)))
        .collect();
    all.iter()
        .rev()
        .find(|(at, _)| *at <= t)
        .or_else(|| all.first())
        .map(|(_, r)| r.clone())
}

/// What `status` says of `r` in its four fields ([`FIELDS`]): `-` for each it does not name.
#[must_use]
pub fn fields_of(r: Option<&LandReceipt>) -> [String; 4] {
    let named = |s: &str| {
        if s.is_empty() {
            "-".to_owned()
        } else {
            s.to_owned()
        }
    };
    match r {
        Some(r) => [
            named(&r.head),
            named(&r.main_ref),
            r.leg.word().to_owned(),
            named(&r.repo),
        ],
        None => ["-", "-", "-", "-"].map(str::to_owned),
    }
}

/// STATUS SAYS WHAT ATERM CHECKED: every `status` read after `t0` whose `task=` is `landed` says
/// the journal's receipt in its four fields ([`fields_of`]; `landed_on=` and `landed_repo=`
/// percent-decoded), every other read `-` in each; a read without them (a build before 9c)
/// fails; each landed read speaks for the landing it follows (`receipt_at`). The last receipt's
/// leg is the one `chain`'s folder makes ([`leg_of`]), and its head the chain's `HEAD` where the
/// lab read it. A chain that lands with no landed read is unobserved.
#[must_use]
pub fn status_agrees(chain: Chain, seen: &Seen, t0: i64) -> Verdict {
    let r = receipt_of(seen, t0);
    let reads: Vec<&(i64, String)> = seen.status.iter().filter(|(t, _)| *t > t0).collect();
    let Some(&(_, last)) = reads.last() else {
        return Verdict::Unobserved(format!("no status read to +{}", at(t0, seen.end_ms)));
    };
    let mut bad = Vec::new();
    let mut landed = 0;
    for (t, line) in &reads {
        let got = FIELDS.map(|k| word(line, k).unwrap_or_else(|| "?".to_owned()));
        if got.iter().any(|g| g == "?") {
            return Verdict::Fail(format!(
                "status at +{} names no landed_head= landed_on= landed_leg= landed_repo=: {}",
                at(t0, *t),
                one_line(line, 160)
            ));
        }
        let got = [
            got[0].clone(),
            pct_decode(&got[1]),
            got[2].clone(),
            pct_decode(&got[3]),
        ];
        let is_landed = word(line, "task").as_deref() == Some("landed");
        landed += usize::from(is_landed);
        let want = fields_of(receipt_at(seen, t0, *t).as_ref().filter(|_| is_landed));
        if got != want {
            bad.push(format!(
                "+{} task={}: {} (wanted {})",
                at(t0, *t),
                word(line, "task").unwrap_or_default(),
                FIELDS
                    .iter()
                    .zip(&got)
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                want.join(" ")
            ));
        }
    }
    if let Some(r) = &r {
        if r.leg != leg_of(chain) {
            bad.push(format!(
                "the receipt's leg is {}, {}'s folder makes {}",
                r.leg.word(),
                chain.name(),
                leg_of(chain).word()
            ));
        }
        let head = seen.head.as_deref().and_then(|h| h.get(..HEAD_DIGITS));
        if let Some(head) = head.filter(|_| r.leg != ReceiptLeg::NoRepo)
            && r.head != head
        {
            bad.push(format!(
                "the receipt names head {}, the chain's HEAD is {head}",
                r.head
            ));
        }
        if r.leg == ReceiptLeg::OnMain && r.main_ref != "origin/main" {
            bad.push(format!(
                "the receipt's ref is {}, not origin/main",
                r.main_ref
            ));
        }
    }
    let lands = chain.lands()
        || chain
            .scenarios()
            .iter()
            .any(|&s| receipt::judged(chain, s) == Some(true));
    let said = format!(
        "{} status read(s), {landed} landed, each {}; the last: {}",
        reads.len(),
        r.as_ref().map_or_else(
            || "landed_head=- (no receipt journaled)".to_owned(),
            |r| format!(
                "landed_head={} landed_on={} landed_leg={} where landed, - elsewhere",
                fields_of(Some(r))[0],
                fields_of(Some(r))[1],
                r.leg.word()
            )
        ),
        FIELDS
            .iter()
            .map(|k| format!("{k}={}", word(last, k).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(" ")
    );
    if !bad.is_empty() {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    } else if lands && (landed == 0 || r.is_none()) {
        Verdict::Unobserved(format!("{said}: no landed read with its receipt"))
    } else {
        Verdict::Pass(said)
    }
}

/// Whether scenario `s` holds its landed chip's title to its receipt ([`title_checked`]): the
/// land chains whose tab the run reads in front once landed (L1, L7, L16, L17, L18, L19).
#[must_use]
pub const fn titled(s: Scenario) -> bool {
    matches!(
        s,
        Scenario::L1 | Scenario::L7 | Scenario::L16 | Scenario::L17 | Scenario::L18 | Scenario::L19
    )
}

/// THE LANDED TITLE SAYS THIS LANDING'S RECEIPT (the review round of slice 9c): every read of the
/// chip's title after `t0` whose chip wears the landed face (the front tab's reads and END's
/// front sweep) holds [`mode_words::landed_brief`] of the receipt that read speaks for
/// (`receipt_at`), or of none where the journal holds none: the receipt's own head, ref and
/// folder, not a lead alone, which is all `forever_surfaces::title_says` reads. A run that read
/// no window skips it by name; one that read no landed title is unobserved.
#[must_use]
pub fn title_checked(seen: &Seen, t0: i64) -> Verdict {
    use crate::forever::surfaces::{chip, split_title};
    let w = &seen.window;
    if !w.read {
        return Verdict::Pass("the landed title SKIPPED (--no-surfaces)".into());
    }
    let mut reads: Vec<(i64, String)> = Vec::new();
    if let Some((t, line)) = &w.front
        && *t > t0
        && let (rest, Some(title)) = split_title(line)
        && chip(&rest).face == "landed"
    {
        reads.push((*t, title));
    }
    for (t, title) in w.title.iter().filter(|(t, _)| *t > t0) {
        let landed = w
            .chip
            .iter()
            .rfind(|(at, _)| at <= t)
            .is_some_and(|(_, line)| chip(line).face == "landed");
        if landed {
            reads.push((*t, title.clone()));
        }
    }
    reads.retain(|(_, title)| title != "-" && title != "?");
    if reads.is_empty() {
        return Verdict::Unobserved(format!(
            "no landed title read in front to +{}",
            at(t0, seen.end_ms)
        ));
    }
    let mut bad = Vec::new();
    for (t, title) in &reads {
        let want = mode_words::landed_brief(receipt_at(seen, t0, *t).as_ref());
        if !title.contains(&want) {
            bad.push(format!(
                "+{}: {} (wanted {want:?})",
                at(t0, *t),
                one_line(title, 240)
            ));
        }
    }
    let (t, last) = &reads[reads.len() - 1];
    if bad.is_empty() {
        Verdict::Pass(format!(
            "{} landed title read(s), each its receipt's words; at +{}: {}",
            reads.len(),
            at(t0, *t),
            one_line(last, 240)
        ))
    } else {
        Verdict::Fail(format!(
            "landed titles not their receipt's: {}",
            bad.join("; ")
        ))
    }
}

/// THE WORKER'S `checked=` (L15): the manager's `await task` names the worker's receipt's
/// `checked=` word on its row, and `result`'s header ends with it.
#[must_use]
pub fn worker_checked(seen: &Seen, t0: i64) -> Verdict {
    let Some(r) = receipt_of(seen, t0) else {
        return Verdict::Unobserved("no RECEIPT journaled".into());
    };
    let word = r.checked_word();
    let field = format!("checked={word}");
    let mut bad = Vec::new();
    let mut said = Vec::new();
    match &seen.cover.awaited {
        Some((_, _, out)) if out.split_whitespace().any(|w| w == field) => {
            said.push(format!("await task says {field}"));
        }
        Some((_, _, out)) => bad.push(format!(
            "await task says no {field}: {}",
            one_line(out, 200)
        )),
        None => bad.push("the manager's await task never returned".into()),
    }
    match &seen.cover.result_head {
        Some(head) if head.trim_end().ends_with(&format!(" {field}")) => {
            said.push(format!("result's header ends {field}"));
        }
        Some(head) => bad.push(format!("result's header ends no {field}: {head}")),
        None => bad.push("no result header read".into()),
    }
    if bad.is_empty() {
        Verdict::Pass(said.join("; "))
    } else {
        Verdict::Fail(format!("{}: {}", said.join("; "), bad.join("; ")))
    }
}

/// `seen` of `chain` as a 9c build journals and prints it, for the fixtures written before
/// `status` said what aterm checked. A chain that carries no receipt want of its own (W2, W4–W6,
/// W9–W11, O1, the switch chains: 9b's departure 7) has each land `DONE` note followed by the
/// `RECEIPT` of its seq, the leg its folder makes ([`leg_of`]) and the head the lab read (or
/// `1c81bc6f0e2a`), its landing on the land check's `LANDED` (slice 13a); then each `status` read without `landed_head=` gains the four fields a 9c
/// build prints, the receipt of the landing it follows ([`fields_of`] of `receipt_at`) where its
/// `task=` is `landed` and `-` elsewhere. A read that carries them is left as it is.
#[cfg(test)]
#[must_use]
pub fn as_built(chain: Chain, seen: &Seen, t0: i64) -> Seen {
    let mut s = seen.clone();
    let wants_none = chain
        .scenarios()
        .iter()
        .all(|&sc| receipt::judged(chain, sc).is_none());
    if wants_none {
        let leg = leg_of(chain);
        let head = seen
            .head
            .as_deref()
            .and_then(|h| h.get(..HEAD_DIGITS))
            .unwrap_or("1c81bc6f0e2a");
        let mut journal = Vec::with_capacity(s.journal.len());
        let lines = std::mem::take(&mut s.journal);
        for (i, (t, l)) in lines.iter().enumerate() {
            journal.push((*t, l.clone()));
            let seq = l
                .split(' ')
                .find_map(|w| w.strip_prefix("seq="))
                .filter(|_| {
                    l.starts_with("DONE ") && l.contains(&format!(" rule={} ", turn_end::RULE_LAND))
                });
            let told = lines
                .get(i + 1)
                .is_some_and(|(_, next)| next.starts_with(&format!("{RECEIPT} ")));
            if let Some(seq) = seq.filter(|_| !told) {
                let named = leg != ReceiptLeg::NoRepo;
                let r = LandReceipt {
                    head: if named {
                        head.to_owned()
                    } else {
                        String::new()
                    },
                    main_ref: if leg == ReceiptLeg::OnMain {
                        "origin/main".into()
                    } else {
                        String::new()
                    },
                    repo: if named {
                        format!("/private/tmp/at.x/proj/{}", chain.name())
                    } else {
                        String::new()
                    },
                    branch: if leg == ReceiptLeg::OnMain {
                        "main".into()
                    } else {
                        String::new()
                    },
                    leg,
                    check: true,
                    said: "Nothing left: the parser".into(),
                    at_ms: *t,
                    statement: aterm_agent::harness::land_receipt::Statement::Unread,
                };
                journal.push((*t, r.line(seq.parse().unwrap_or(0))));
            }
        }
        s.journal = journal;
    }
    let receipted = s.clone();
    for (t, line) in &mut s.status {
        if word(line, FIELDS[0]).is_some() {
            continue;
        }
        let landed = word(line, "task").as_deref() == Some("landed");
        let r = receipt_at(&receipted, t0, *t).filter(|_| landed);
        let f = fields_of(r.as_ref());
        let pct = |v: &str| {
            if v == "-" {
                v.to_owned()
            } else {
                aterm_codec::pct::encode(v)
            }
        };
        line.push_str(&format!(
            " landed_head={} landed_on={} landed_leg={} landed_repo={}",
            f[0],
            pct(&f[1]),
            f[2],
            pct(&f[3])
        ));
    }
    s
}

/// L18, L19: Land's instruction typed, then a land `DONE` after it, and `status` said
/// `task=landed`.
#[must_use]
pub fn judge_landing(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Fail(format!("no land@v1 typed within {}", at(t0, seen.end_ms)));
    };
    let Some((d, line)) = seen.first_after("DONE ", *t1) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the land act at +{}",
            at(t0, seen.end_ms),
            at(t0, *t1)
        ));
    };
    let said = format!(
        "land@v1 at +{}, DONE at +{} ({})",
        at(t0, *t1),
        at(t0, *d),
        one_line(line, 90)
    );
    if !line.contains(&format!("rule={}", turn_end::RULE_LAND)) {
        return Verdict::Fail(format!("{said}: the DONE names another rule"));
    }
    if !seen.status_said("task", "landed", d - 5000) {
        return Verdict::Fail(format!("{said}: status never said task=landed"));
    }
    Verdict::Pass(said)
}

#[cfg(test)]
#[path = "forever_checked_tests.rs"]
mod tests;
