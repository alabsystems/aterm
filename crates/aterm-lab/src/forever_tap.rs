// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TAP TIMES THE WAIT (`docs/DESIGN-claude-accounts-v2-2026-10-15.md` §3.3): what `lab
//! forever --mode wall-tap` and `--mode wall-tap-narrow` want of a real Claude Code's five-hour
//! wall when its usage tap reads the window.
//!
//! * The tab's Claude Code runs with aterm's usage tap on its line, as the managed twin's launch
//!   carries it ([`tap_args`]), so its status line writes the window's figures where the instance
//!   reads them (`IdleHost::tap_window`).
//! * The stub's prearmed five-hour wall answers `429` with the notice's reset
//!   (`anthropic-ratelimit-unified-reset`, the stub's reset `R`) and the window's own figures:
//!   full, its reset [`TAP_LATER_S`] past the notice's ([`on_429`], the stub's `@RESET+N@`), within
//!   `CLOCK_SLACK_S`, so the loop takes the tap's reset.
//! * W10 (`wall-tap`): once the loop waits on the wall, the lab presses Esc on Claude Code's
//!   countdown, so no continue of its own comes; once the loop decided the wait past the cancel,
//!   the lab changes one benign `[harness]` key live ([`RESTART_KEY`]), and the instance hands the
//!   session to a new loop, which takes the wait over ([`step`]). The stub lifts its wall at `R`.
//!   [`judge`] wants, beside W1's judge:
//!   - the first loop's `wall-wait@v1` rows naming the tap's reset, `tap_unix=R+`[`TAP_LATER_S`],
//!     from the first that names one;
//!   - the new loop (journaling past the first loop's `EXIT`) reading the turn's zone-named
//!     notice, Claude Code's countdown and footer gone (`10:58pm (America/Los_Angeles)` where the
//!     first loop read the footer's `10:58pm`): one of its `wall-wait@v1` rows names that zone,
//!     and every one names the tap's reset; a new loop that never read it is UNOBSERVED;
//!   - the wait past the cancel journaled to the tap's reset plus `reset_grace` plus the session's
//!     share of `reset_jitter`, the new loop's first wait and every later wait to the reset up to
//!     the resume at that same point;
//!   - the continuation (`usage-resume@v1`) typed at that point, not at the notice's reset plus
//!     the grace.
//! * W11 (`wall-tap-narrow`): once the loop waits on the wall, its countdown armed, the lab
//!   narrows the tab to [`NARROW`]'s 16 columns, where Claude Code wraps its countdown's words
//!   inside a word, and widens it again once the loop decided its wait there ([`step`]). Claude
//!   Code goes on by itself past `R`. [`judge_narrow`] wants, beside W1's judge, the wait decided
//!   in the narrow pane timed by the tap (its reset plus the grace and the share, or plus the
//!   countdown's grace) and every `wall-wait@v1` row written there naming `tap_unix=`.
//!
//! `--untapped` is the live negative control: the same run with no tap on the line, whose wait
//! goes to the notice's reset, so W10 and W11 must FAIL.

use aterm_agent::harness::tap;
use aterm_agent::supervise::approvals::RULE_WALL_WAIT;
use aterm_agent::supervise::policy::turn_end::{self, RULE_LIMIT_RESUME};

use super::{CLAUDE_CANCEL, Chain, Harness, Seen, Tab, Verdict, act_rule, at, on, word};
use crate::Fail;
use crate::door::Door;
use crate::run::Run;
use crate::util::{now_ms, one_line};

/// How far past the notice's reset the window's own reset is (seconds): inside the loop's clock
/// slack (6 min), so the tap's is taken, and far enough that a wait to the notice's reset cannot
/// pass for it, whatever the two sessions' shares of the jitter.
pub const TAP_LATER_S: i64 = 120;

/// The stub's headers on the wall's 429 ([`crate::stub::reset_value`]'s `@RESET+N@`): the
/// five-hour window full, its reset [`TAP_LATER_S`] past the notice's.
#[must_use]
pub fn on_429() -> String {
    format!(
        r#"{{"on429":{{"anthropic-ratelimit-unified-5h-status":"rejected","anthropic-ratelimit-unified-5h-utilization":"1.0","anthropic-ratelimit-unified-5h-reset":"@RESET+{TAP_LATER_S}@"}}}}"#
    )
}

/// The `[harness]` key the lab changes live to start a new loop on the session mid-wait: the
/// person's grace, a second longer than its default, which no act of the chain waits on.
pub const RESTART_KEY: (&str, &str) = ("human_grace_s", "121");

/// How long past the cancel the lab waits for the loop's new wait before it gives up.
const WAIT_DECIDED_MS: i64 = 60_000;

/// How long the successor's wait may come after the key's change.
const SUCCESSOR_MS: i64 = 60_000;

/// How far either side of its due the continuation may be typed: the loop's wake and the lab's
/// two-second looks.
const DUE_SLACK_MS: i64 = 5_000;

/// The claude arguments that put aterm's usage tap on the line (`--settings`, the managed twin's
/// value), or why they cannot.
/// # Errors
/// The lab's aterm cannot be named in the tap's command ([`tap::settings_value`]).
pub fn tap_args(run: &Run) -> Result<Vec<String>, Fail> {
    let value = tap::settings_value(std::path::Path::new(&run.aterm)).ok_or_else(|| {
        super::could_not(&format!(
            "the usage tap: {} cannot be named in its command",
            run.aterm
        ))
    })?;
    Ok(vec!["--settings".to_string(), value])
}

/// The narrow pane W11 puts the wall in, `(rows, cols)`: at 16 columns Claude Code 2.1.314
/// breaks its countdown's `automatically` inside the word (`automatic` over `ally`), while the
/// footer's `limit resets 11:11pm` still wraps whole (measured 2026-10-30).
pub const NARROW: (u16, u16) = (50, 16);

/// What the lab did to W10's or W11's wall ([`step`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Driven {
    /// W10: when Esc went in on Claude Code's countdown.
    pub cancelled_ms: Option<i64>,
    /// W10: when the lab changed [`RESTART_KEY`], handing the session to a new loop.
    pub restarted_ms: Option<i64>,
    /// W11: when the lab narrowed the tab to [`NARROW`].
    pub narrowed_ms: Option<i64>,
    /// W11: when it gave the tab its own size back.
    pub widened_ms: Option<i64>,
    /// Why the lab stopped driving, where it did.
    pub gave_up: Option<String>,
}

impl Driven {
    /// Record the restore's actual reply; a refused resize completes no widening.
    pub(super) fn after_widening(mut self, reply: &crate::door::Reply, at: i64) -> Self {
        if reply.code == 0 {
            self.widened_ms = Some(at);
        } else {
            self.gave_up = Some(format!(
                "restoring the tab's width refused (exit {}): {}{}",
                reply.code,
                reply.stdout.trim(),
                reply.stderr.trim()
            ));
        }
        self
    }
}

/// The journal's wall waits from `from` on: `WAITING … until=<when> usage-session resets`, each
/// with its due (unix ms).
fn resets_waits(seen: &Seen, from: i64) -> Vec<(i64, i64, &str)> {
    wall_waits(seen, from, &[" resets"])
}

/// The journal's waits from `from` on whose words end in one of `ends`, each with its due (unix
/// ms).
fn wall_waits<'a>(seen: &'a Seen, from: i64, ends: &[&str]) -> Vec<(i64, i64, &'a str)> {
    seen.journal
        .iter()
        .filter(|(t, l)| {
            *t > from && l.starts_with("WAITING ") && ends.iter().any(|e| l.ends_with(e))
        })
        .filter_map(|(t, l)| {
            let (_, rest) = l.split_once(" until=")?;
            let due = crate::workers::iso_ms(rest.split_whitespace().next()?)?;
            Some((*t, due, l.as_str()))
        })
        .collect()
}

/// The waits on a wall the loop journals: to its reset (`… resets`), or to Claude Code's own
/// continue (`… goes on by itself`).
const WALL_WAIT_ENDS: [&str; 2] = [" resets", " goes on by itself"];

/// Whether the loop waits on the wall the land act met: a wait to its reset or to Claude Code's
/// own continue journaled after that act. The decision past the act's take window comes at a
/// re-read of its own `seq=`, which journals no point of its own, so the wait's `seq=` need not
/// be the wall's point's.
pub(super) fn waits_on_the_wall(seen: &Seen) -> bool {
    seen.acts(turn_end::RULE_LAND)
        .first()
        .is_some_and(|(t1, _)| !wall_waits(seen, *t1, &WALL_WAIT_ENDS).is_empty())
}

fn say(tab: &Tab, what: &str) {
    println!(
        "  [{}] +{} tap     {what}",
        tab.chain.name(),
        at(tab.t0, now_ms())
    );
}

/// The tapped walls' drive ([`Chain::tapped`]): W10's Esc and new loop, W11's narrow pane.
/// # Errors
/// A key or a write the lab could not make.
pub(super) fn step(run: &Run, door: &Door<'_>, h: Harness, tab: &mut Tab) -> Result<(), Fail> {
    if h != Harness::Claude {
        return Ok(());
    }
    match tab.chain {
        Chain::TapWall => esc_step(run, door, tab),
        Chain::NarrowTapWall => narrow_step(run, door, tab),
        _ => Ok(()),
    }
}

/// W11's drive: the tab narrowed to [`NARROW`] once the loop waits on the wall with Claude Code's
/// countdown armed, then given its own size back once the loop decided a wait there.
fn narrow_step(run: &Run, door: &Door<'_>, tab: &mut Tab) -> Result<(), Fail> {
    let Some((_, reset_s)) = tab.seen.cover.wall else {
        return Ok(());
    };
    let driven = tab.seen.cover.tap.clone().unwrap_or_default();
    if driven.gave_up.is_some() || driven.widened_ms.is_some() {
        return Ok(());
    }
    let who = format!("@{}", tab.sid);
    let deadline = reset_s * 1000 - super::HIDE_BEFORE_RESET_MS;
    let Some(narrowed) = driven.narrowed_ms else {
        if !waits_on_the_wall(&tab.seen) {
            return Ok(());
        }
        let text = on(door, &tab.sid, &["text"]);
        let d = if now_ms() >= deadline {
            Driven {
                gave_up: Some("the reset under 2 min away before the pane was narrowed".into()),
                ..Driven::default()
            }
        } else if !super::countdown_armed(&text, CLAUDE_CANCEL) {
            Driven {
                gave_up: Some("no countdown armed: the pane not narrowed".into()),
                ..Driven::default()
            }
        } else {
            let (rows, cols) = NARROW;
            let r = door.ctl(&[&who, "resize", &rows.to_string(), &cols.to_string()])?;
            if r.code == 0 {
                say(
                    tab,
                    &format!("narrowed to {cols} columns ({})", r.stdout.trim()),
                );
                Driven {
                    narrowed_ms: Some(now_ms()),
                    ..Driven::default()
                }
            } else {
                Driven {
                    gave_up: Some(format!(
                        "`resize {rows} {cols}` refused: {}",
                        r.stderr.trim()
                    )),
                    ..Driven::default()
                }
            }
        };
        if let Some(why) = &d.gave_up {
            say(tab, &format!("NOT driven: {why}"));
        }
        tab.seen.cover.tap = Some(d);
        return Ok(());
    };
    let now = now_ms();
    let decided = !wall_waits(&tab.seen, narrowed, &WALL_WAIT_ENDS).is_empty();
    if !decided && now - narrowed <= WAIT_DECIDED_MS && now < deadline {
        return Ok(());
    }
    let r = door.ctl(&[&who, "resize", &run.rows, &run.cols])?;
    let driven = driven.after_widening(&r, now_ms());
    if let Some(why) = &driven.gave_up {
        say(tab, &format!("NOT widened: {why}"));
        tab.seen.cover.tap = Some(driven);
        return Ok(());
    }
    say(
        tab,
        &format!(
            "{} the tab's own {} columns again ({})",
            if decided {
                "a wait decided in the narrow pane:"
            } else {
                "NO wait decided in the narrow pane;"
            },
            run.cols,
            r.stdout.trim()
        ),
    );
    tab.seen.cover.tap = Some(driven);
    Ok(())
}

/// W10's drive: Esc on the countdown once the loop waits on the wall, then the key's change once
/// the loop decided its wait past the cancel.
fn esc_step(run: &Run, door: &Door<'_>, tab: &mut Tab) -> Result<(), Fail> {
    let Some((_, reset_s)) = tab.seen.cover.wall else {
        return Ok(());
    };
    let driven = tab.seen.cover.tap.clone().unwrap_or_default();
    if driven.gave_up.is_some() || driven.restarted_ms.is_some() {
        return Ok(());
    }
    let deadline = reset_s * 1000 - super::HIDE_BEFORE_RESET_MS;
    let Some(cancelled) = driven.cancelled_ms else {
        if !waits_on_the_wall(&tab.seen) {
            return Ok(());
        }
        let at_tab = format!("@{}", tab.sid);
        let d = match super::cancel_countdown(door, &at_tab, &tab.sid, CLAUDE_CANCEL, deadline)? {
            Ok(at) => {
                say(tab, "its countdown cancelled (escape)");
                Driven {
                    cancelled_ms: Some(at),
                    ..Driven::default()
                }
            }
            Err(why) => {
                say(tab, &format!("NOT driven: {why}"));
                Driven {
                    gave_up: Some(why),
                    ..Driven::default()
                }
            }
        };
        tab.seen.cover.tap = Some(d);
        return Ok(());
    };
    let now = now_ms();
    if resets_waits(&tab.seen, cancelled).is_empty() {
        if now - cancelled > WAIT_DECIDED_MS || now >= deadline {
            let why = format!(
                "no wait to the reset journaled within {} of the cancel",
                at(0, now - cancelled)
            );
            say(tab, &format!("NOT restarted: {why}"));
            tab.seen.cover.tap = Some(Driven {
                gave_up: Some(why),
                ..driven
            });
        }
        return Ok(());
    }
    let (k, v) = RESTART_KEY;
    crate::verbs::set_harness(run, &[(k.to_string(), v.to_string())])?;
    let at_key = now_ms();
    say(
        tab,
        &format!("[harness] {k} = {v} written live: the session is handed to a new loop"),
    );
    tab.seen.cover.tap = Some(Driven {
        restarted_ms: Some(at_key),
        ..driven
    });
    Ok(())
}

/// The `wall-wait@v1` rows written from `from` through `until` (when set).
fn wall_rows(seen: &Seen, from: i64, until: Option<i64>) -> Vec<&super::Row> {
    seen.ledger
        .iter()
        .filter(|r| {
            r.ts >= from && until.is_none_or(|until| r.ts <= until) && r.rule == RULE_WALL_WAIT
        })
        .collect()
}

/// The reset a `wall-wait@v1` row names as the tap's (`tap_unix=`, unix seconds).
fn tap_of(row: &super::Row) -> Option<i64> {
    row.reason.split_once("; tap_unix=").and_then(|(_, rest)| {
        rest.split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    })
}

/// Whether a `wall-wait@v1` row's reset words (`reset=`) name their zone, as Claude Code's turn
/// notice prints them (`10:58pm (America/Los_Angeles)`) and its footer does not (`10:58pm`).
fn names_its_zone(row: &super::Row) -> bool {
    row.reason
        .split_once("; reset=")
        .and_then(|(_, rest)| rest.split_once("; kind="))
        .is_some_and(|(words, _)| words.trim_end().ends_with(')') && words.contains(" ("))
}

/// The faults of `rows` that name no tap's reset or another than `tap_reset` (unix ms), `whose`
/// naming the loop or the pane that wrote them; the first of each kind, with how many.
fn untapped_rows(rows: &[&super::Row], tap_reset: i64, whose: &str) -> Vec<String> {
    let mut faults = Vec::new();
    let none: Vec<&&super::Row> = rows.iter().filter(|r| tap_of(r).is_none()).collect();
    if let Some(r) = none.first() {
        faults.push(format!(
            "{} of {whose} wall-wait@v1 row(s) name NO tap's reset (no tap_unix=): {}",
            none.len(),
            one_line(&r.reason, 160)
        ));
    }
    let other: Vec<&&super::Row> = rows
        .iter()
        .filter(|r| tap_of(r).is_some_and(|t| t * 1000 != tap_reset))
        .collect();
    if let Some(r) = other.first() {
        faults.push(format!(
            "{} of {whose} wall-wait@v1 row(s) name tap_unix={}, not the window's {} ({})",
            other.len(),
            tap_of(r).unwrap_or_default(),
            tap_reset / 1000,
            one_line(&r.reason, 160)
        ));
    }
    faults
}

/// Judges W10 ([`Chain::TapWall`]): W1's judge, and the wait timed by the tap.
pub(super) fn judge(seen: &Seen, land: Option<&(i64, String)>, program: &str, t0: i64) -> Verdict {
    let w1 = super::cover::judge_wall(seen, land, program, t0);
    let (said, faults) = match tap_facts(seen, t0) {
        Ok(f) => f,
        Err(why) if matches!(w1, Verdict::Fail(_)) => {
            return Verdict::Fail(format!("{why}; W1's judge: {}", w1.why()));
        }
        Err(why) => {
            return Verdict::Unobserved(format!("{why}; W1's judge {}: {}", w1.word(), w1.why()));
        }
    };
    compose(said, &faults, w1)
}

/// What W10's records say of the tap's timing, and its faults; `Err` where the run did not drive
/// or show what W10 judges and no fault was found.
///
/// The loop the key's change starts is the one journaling after the first `EXIT` past the change
/// (the loop before it stopping). W10 judges that new loop across the reset's key change: its
/// `wall-wait@v1` rows must include one from the turn's zone-named notice (else the change was
/// not met: UNOBSERVED) and every one must name the tap's reset, and every wait to the reset
/// either loop journaled past the cancel, to the resume, must be the tap's due.
fn tap_facts(seen: &Seen, t0: i64) -> Result<(String, Vec<String>), String> {
    let Some((raised, reset_s)) = seen.cover.wall else {
        return Err("the stub's wall was never raised".to_string());
    };
    let reset = reset_s * 1000;
    let driven = seen.cover.tap.clone().unwrap_or_default();
    if let Some(why) = &driven.gave_up {
        return Err(format!("the wall was not driven: {why}"));
    }
    let Some(cancelled) = driven.cancelled_ms else {
        return Err("its countdown was never cancelled".to_string());
    };
    let Some(restarted) = driven.restarted_ms else {
        return Err("no new loop was started in the wait".to_string());
    };
    let timing = turn_end::TurnEndTiming::default();
    let ms = |d: std::time::Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    let (grace, jitter) = (ms(timing.reset_grace), ms(timing.reset_jitter));
    let tap_reset = reset + TAP_LATER_S * 1000;
    let from = reset + grace;
    let handed = seen
        .journal
        .iter()
        .find(|(t, l)| *t > restarted && (l == "EXIT" || l.starts_with("EXIT ")))
        .map(|(t, _)| *t);
    let mut faults = Vec::new();
    let mut unseen = Vec::new();
    let mut said = format!(
        "the notice's reset +{}, the window's +{} (tap_unix={}); Esc on the countdown at +{}, a new loop at +{}{}",
        at(t0, reset),
        at(t0, tap_reset),
        tap_reset / 1000,
        at(t0, cancelled),
        at(t0, restarted),
        handed.map_or_else(String::new, |h| format!(
            " (the loop before it gone at +{})",
            at(t0, h)
        ))
    );
    let first_rows = wall_rows(seen, raised, handed);
    match first_rows.iter().position(|r| tap_of(r).is_some()) {
        Some(i) => {
            let wrong = untapped_rows(&first_rows[i..], tap_reset, "the first loop's");
            if wrong.is_empty() {
                said.push_str(&format!(
                    "; wall-wait@v1 names tap_unix={}",
                    tap_reset / 1000
                ));
            }
            faults.extend(wrong);
        }
        None => faults.push(first_rows.first().map_or_else(
            || "NO wall-wait@v1 row after the wall was raised".to_string(),
            |r| {
                format!(
                    "NO wall-wait@v1 row names the tap's reset (no tap_unix=): {}",
                    one_line(&r.reason, 160)
                )
            },
        )),
    }
    match handed {
        None => unseen.push(
            "no loop's EXIT journaled past the key's change: no new loop was seen taking the wait over"
                .to_string(),
        ),
        Some(h) => {
            let theirs = wall_rows(seen, h, None);
            let wrong = untapped_rows(&theirs, tap_reset, "the new loop's");
            if !theirs.iter().any(|r| names_its_zone(r)) {
                unseen.push(format!(
                    "none of the new loop's {} wall-wait@v1 row(s) names its reset's zone: it never read the turn's zone-named notice, the key change W10 checks",
                    theirs.len()
                ));
            } else if wrong.is_empty() {
                said.push_str(&format!(
                    "; the new loop's {} wall-wait@v1 row(s), the zone-named notice's among them, name it too",
                    theirs.len()
                ));
            }
            faults.extend(wrong);
        }
    }
    let resumed = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t >= reset)
        .map(|(t, l)| (*t, act_rule(l).unwrap_or("?").to_string()));
    let in_share = |due: i64| due >= tap_reset + grace && due < tap_reset + grace + jitter;
    let waits: Vec<(i64, i64, &str)> = resets_waits(seen, cancelled)
        .into_iter()
        .filter(|(t, ..)| resumed.as_ref().is_none_or(|(ta, _)| t < ta))
        .collect();
    let last_before = waits.iter().rposition(|(t, ..)| *t <= restarted);
    let first_after = handed.and_then(|h| waits.iter().position(|(t, ..)| *t > h));
    let mut due = None;
    match last_before.map(|i| &waits[i]) {
        Some((t, d, _)) if in_share(*d) => {
            due = Some(*d);
            said.push_str(&format!(
                "; the wait past the cancel (+{}) to +{}: the tap's reset, {} grace and a {} share",
                at(t0, *t),
                at(t0, *d),
                at(0, grace),
                at(0, d - tap_reset - grace)
            ));
        }
        Some((t, d, l)) => faults.push(format!(
            "the wait past the cancel (+{}) is to +{}, not the tap's reset +{} with the grace and a share under {}: {}",
            at(t0, *t),
            at(t0, *d),
            at(t0, tap_reset),
            at(0, jitter),
            one_line(l, 120)
        )),
        None => faults.push("no wait to the reset journaled past the cancel".to_string()),
    }
    let right = |d: i64| in_share(d) && due.is_none_or(|due| (d - due).abs() <= 1_000);
    let tap_due = || due.map_or_else(|| "?".to_string(), |d| format!("+{}", at(t0, d)));
    if handed.is_some() {
        match first_after
            .map(|i| &waits[i])
            .filter(|(t, ..)| *t <= restarted + SUCCESSOR_MS)
        {
            Some((t, d, _)) if right(*d) => {
                said.push_str(&format!(
                    "; the new loop's wait (+{}) to the same +{}",
                    at(t0, *t),
                    at(t0, *d)
                ));
            }
            Some((t, d, l)) => faults.push(format!(
                "the new loop's wait (+{}) is to +{}, not the tap's {}: {}",
                at(t0, *t),
                at(t0, *d),
                tap_due(),
                one_line(l, 120)
            )),
            None => faults.push(format!(
                "the new loop journaled no wait to the reset within {} of the key's change",
                at(0, SUCCESSOR_MS)
            )),
        }
    }
    let off: Vec<&(i64, i64, &str)> = waits
        .iter()
        .enumerate()
        .filter(|(i, (_, d, _))| Some(*i) != last_before && Some(*i) != first_after && !right(*d))
        .map(|(_, w)| w)
        .collect();
    if let Some((t, d, l)) = off.first() {
        faults.push(format!(
            "{} other wait(s) to the reset past the cancel off the tap's {}: {} at +{} to +{}: {}",
            off.len(),
            tap_due(),
            if handed.is_some_and(|h| t > &h) {
                "the new loop's"
            } else {
                "the loop before its"
            },
            at(t0, *t),
            at(t0, *d),
            one_line(l, 120)
        ));
    }
    match (resumed, due) {
        (Some((ta, rule)), Some(due))
            if rule == RULE_LIMIT_RESUME && (ta - due).abs() <= DUE_SLACK_MS =>
        {
            said.push_str(&format!("; {rule} typed at +{}, at its due", at(t0, ta)));
        }
        (Some((ta, rule)), _) if ta < tap_reset + grace => faults.push(format!(
            "{rule} TYPED at +{}, before the tap's reset and the grace (+{}): {} past the notice's",
            at(t0, ta),
            at(t0, tap_reset + grace),
            at(reset, ta)
        )),
        (Some((ta, rule)), _) => faults.push(format!(
            "{rule} typed at +{}, not {RULE_LIMIT_RESUME} at the wait's due {}",
            at(t0, ta),
            due.map_or_else(|| "?".to_string(), |d| format!("+{}", at(t0, d)))
        )),
        (None, _) if seen.end_ms < from + jitter + TAP_LATER_S * 1000 + DUE_SLACK_MS => {
            unseen.push(format!(
                "watched to +{}, before the tap's due",
                at(t0, seen.end_ms)
            ));
        }
        (None, _) => faults.push("nothing typed past the reset".to_string()),
    }
    if let Some((t, l)) = seen.journal.iter().find(|(t, l)| {
        *t > cancelled
            && word(l, "rule").as_deref() == Some(turn_end::RULE_LIMIT_RESUME)
            && *t < from
    }) {
        faults.push(format!(
            "the resume journaled at +{}, before the notice's reset and the grace: {}",
            at(t0, *t),
            one_line(l, 100)
        ));
    }
    if !unseen.is_empty() {
        said.push_str(&format!("; {}", unseen.join("; ")));
        if faults.is_empty() {
            return Err(said);
        }
    }
    Ok((said, faults))
}

/// Judges W11 ([`Chain::NarrowTapWall`]): W1's judge, and the wait decided in the narrow pane
/// timed by the tap.
pub(super) fn judge_narrow(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let w1 = super::cover::judge_wall(seen, land, program, t0);
    let (said, faults) = match narrow_facts(seen, t0) {
        Ok(f) => f,
        Err(why) if matches!(w1, Verdict::Fail(_)) => {
            return Verdict::Fail(format!("{why}; W1's judge: {}", w1.why()));
        }
        Err(why) => {
            return Verdict::Unobserved(format!("{why}; W1's judge {}: {}", w1.word(), w1.why()));
        }
    };
    compose(said, &faults, w1)
}

/// W1's verdict `w1` beside a tap judge's facts `said` and its `faults`.
fn compose(said: String, faults: &[String], w1: Verdict) -> Verdict {
    let faulted = if faults.is_empty() {
        String::new()
    } else {
        format!(": {}", faults.join("; "))
    };
    match (w1, faults.is_empty()) {
        (Verdict::Fail(w), _) => Verdict::Fail(format!("{said}{faulted}; W1's judge: {w}")),
        (_, false) => Verdict::Fail(format!("{said}{faulted}")),
        (Verdict::Unobserved(w), true) => Verdict::Unobserved(format!("{said}; W1's judge: {w}")),
        (Verdict::Pass(w), true) => Verdict::Pass(format!("{said}; W1's judge: {w}")),
    }
}

/// What W11's records say of the wait decided in the narrow pane, and its faults; `Err` where the
/// run did not narrow the pane or decide a wait there.
fn narrow_facts(seen: &Seen, t0: i64) -> Result<(String, Vec<String>), String> {
    let Some((_, reset_s)) = seen.cover.wall else {
        return Err("the stub's wall was never raised".to_string());
    };
    let reset = reset_s * 1000;
    let driven = seen.cover.tap.clone().unwrap_or_default();
    if let Some(why) = &driven.gave_up {
        return Err(format!("the wall was not driven: {why}"));
    }
    let (Some(narrowed), Some(widened)) = (driven.narrowed_ms, driven.widened_ms) else {
        return Err("the pane was not narrowed and widened again in the wait".to_string());
    };
    let timing = turn_end::TurnEndTiming::default();
    let ms = |d: std::time::Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    let (grace, jitter, armed) = (
        ms(timing.reset_grace),
        ms(timing.reset_jitter),
        ms(timing.auto_resume_grace),
    );
    let tap_reset = reset + TAP_LATER_S * 1000;
    let mut faults = Vec::new();
    let mut said = format!(
        "the notice's reset +{}, the window's +{} (tap_unix={}); narrowed to {} columns at +{}, widened at +{}",
        at(t0, reset),
        at(t0, tap_reset),
        tap_reset / 1000,
        NARROW.1,
        at(t0, narrowed),
        at(t0, widened)
    );
    let waits: Vec<(i64, i64, &str)> = wall_waits(seen, narrowed, &WALL_WAIT_ENDS)
        .into_iter()
        .filter(|(t, ..)| *t <= widened)
        .collect();
    let Some(&(t, due, line)) = waits.first() else {
        return Err(format!(
            "{said}: no wait on the wall decided in the narrow pane"
        ));
    };
    let by_tap = if line.ends_with(" goes on by itself") {
        due == tap_reset + armed
    } else {
        due >= tap_reset + grace && due < tap_reset + grace + jitter
    };
    if by_tap {
        said.push_str(&format!(
            "; the wait decided there (+{}) to +{}, the tap's: {}",
            at(t0, t),
            at(t0, due),
            one_line(line, 100)
        ));
    } else {
        faults.push(format!(
            "the wait decided in the narrow pane (+{}) is to +{}, not timed by the tap's reset +{}: {}",
            at(t0, t),
            at(t0, due),
            at(t0, tap_reset),
            one_line(line, 120)
        ));
    }
    let rows = wall_rows(seen, narrowed, Some(widened));
    let wrong = untapped_rows(&rows, tap_reset, "the narrow pane's");
    if rows.is_empty() {
        faults.push("NO wall-wait@v1 row written in the narrow pane".to_string());
    } else if wrong.is_empty() {
        said.push_str(&format!(
            "; its {} wall-wait@v1 row(s) there name tap_unix={}",
            rows.len(),
            tap_reset / 1000
        ));
    }
    faults.extend(wrong);
    Ok((said, faults))
}
