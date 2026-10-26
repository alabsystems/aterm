// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `claude-login-proof`: the real Claude Code's lost login on its DEFAULT route, end to end, on a
//! private headless lab instance built from this tree (SPECS §20).
//!
//! The lab lays a dead-login world in its scratch HOME (no `apiKeyHelper`, no OAuth token, a
//! `.credentials.json` whose tokens are empty and long expired) and launches the installed Claude
//! Code by path with no `CLAUDE_CONFIG_DIR`, no `ANTHROPIC_BASE_URL` and no proxy, so it would
//! reach Anthropic but for the lab's jail, which denies every connection but loopback. Four
//! scenarios run on their own tabs:
//!
//! - **S1**: a lost login a roster row names (`lab-a`): the tab waits on aterm's own sign-in of
//!   the row, no `/login` typed, nobody told; the sign-in ladder finds no route (no mailbox, no
//!   profile in the scratch HOME) and records so.
//! - **S2**: a lost login no row names: aterm types `/login` once, presses nothing in its method
//!   picker, and tells the owner (the `login-choose` notice).
//! - **S3**: `aterm harness accounts add lab-b` names that login while the picker stands: aterm
//!   backs out of its own picker with one fenced Esc, no method picked, before the host's own
//!   look at the roster could have told it of the row (the add's wake brought the news), the
//!   tab then waits on the row's sign-in, and the login exception's `CLEARED` line names the
//!   back-out.
//! - **S3b**: a person's own `/login` in the same tab: no key reaches its picker for longer than
//!   the grace the supervisor keeps after a person's keys, and nothing approves it.
//!
//! A process watcher looks for `auth login` and any browser or `open` launch of the lab for the
//! whole run; the open recorder, the egress witness, the keychain tripwire and the jail's own
//! rules are judged too. The lab never presses a key in a `/login` picker: each key it sends is
//! fenced on the composer's own row and pressed once (S1's and S2's prompts and their Enters,
//! S3b's `/login` and its Enter), and the picker S3b opens is left standing until `down` closes
//! the tab. Judges are pure functions of the evidence's text ([`judge_s1`] and the rest).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::accounts::{self, LOGIN_ENV, Roster, Row, Store};
use aterm_agent::harness::headless_oauth::{MAILBOX_TOML_FILE, SECRETS_MAILBOX_TOML_FILE};
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::policy::exception::LOGIN_BACKED_OUT;
use aterm_agent::supervise::policy::guard::row_guard;
use aterm_agent::supervise::policy::login::RULE_LOGIN_BACK_OUT;
use aterm_agent::supervise::policy::notice::LOGIN_CHOOSE;
use aterm_agent::supervise::policy::turn_end::{RULE_LOGIN, TurnEndTiming};
use aterm_agent::supervise::tape::codec::{self, Item};
use aterm_agent::supervise::tape::{corpus, doors, show};

use crate::codex_proof::Verdict;
use crate::door::Door;
use crate::lab::UpOpts;
use crate::pyjson::J;
use crate::run::Run;
use crate::util::{atomic_write, now_ms, scratch_text, shell_quote};
use crate::{Fail, Res, stop};

/// The roster row S1's login is (`lab-a`), listed before any tab runs.
pub const LABEL_A: &str = "lab-a";
/// The row S3 adds for S2's login (`lab-b`), by `aterm harness accounts add`.
pub const LABEL_B: &str = "lab-b";
/// The email of [`LABEL_B`]'s row.
const EMAIL_B: &str = "nobody-b@example.test";
/// What S1 types: any prompt, which meets the dead login.
const PROMPT_A: &str = "Write one sentence about lighthouses, then stop.";
/// What S2 types.
const PROMPT_B: &str = "Write one sentence about harbours, then stop.";
/// A wait for something that must happen: a minute in which nothing it watches moved.
const HANG: Duration = Duration::from_secs(60);
/// The longest any one wait runs however much moves (Claude Code's spinner moves forever).
const WAIT_CAP: Duration = Duration::from_secs(300);
/// How often the host looks at the roster's bytes by itself (aterm-gui's `WAKE_FALLBACK`, the
/// activation thread's wait): the looks come this far apart from the activation wake's arming
/// ([`WAKE_ARMED`]), and one that finds the roster changed logs [`ROSTER_LOOK`].
const HOST_LOOK_EVERY: Duration = Duration::from_secs(60);
/// S3's add lands at most this long after one of the host's looks at the roster ([`add_wait`]),
/// so the next look is `HOST_LOOK_EVERY - S3_ADD_AFTER_LOOK` or more after it: a back-out the
/// add's wake brought comes before that look, one the look brought after it.
const S3_ADD_AFTER_LOOK: Duration = Duration::from_secs(15);
/// How far past a look of the host's an add that waited for it lands.
const LOOK_SETTLE: Duration = Duration::from_secs(2);
/// aterm.log's line for the activation thread's start, its first look at the roster.
pub const WAKE_ARMED: &str = "harness: the activation wake is armed";
/// aterm.log's line for a look of the host's at the roster that found its bytes changed.
pub const ROSTER_LOOK: &str = "harness: the accounts' roster changed: news of the accounts";
/// What S3b watches past the supervisor's grace ([`s3b_grace`]).
const S3B_MARGIN: Duration = Duration::from_secs(15);
/// The word the process watcher's control carries ([`Watcher`]).
const PROC_CONTROL: &str = "lab-login-proof-control";
/// The dead stored login every launch reads: no token, expired at the epoch.
const DEAD_LOGIN: &str = "{\"claudeAiOauth\":{\"accessToken\":\"\",\"refreshToken\":\"\",\"expiresAt\":0,\
                          \"scopes\":[\"user:inference\",\"user:profile\"],\"subscriptionType\":null}}\n";
/// The empty composer's row, as the loop's own fence names it.
const EMPTY_COMPOSER: &str = r"if=^❯\s*$";
/// The composer's row holding `/login` (Claude Code draws a no-break space after `❯`).
const LOGIN_COMPOSER: &str = r"if=^❯\x{A0}/login\s*$";
/// A row of Claude Code's `/login` method picker, which the lab waits for and never presses.
const PICKER_ROW: &str = "Claude account with subscription";

// ---------------------------------------------------------------------------------------------
// The evidence, read from text.

/// One record of a tab's harness journal (`drive/<sid>.journal.jsonl`): when, in ms since the
/// epoch, and its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    /// `t`.
    pub t_ms: i64,
    /// `line`.
    pub line: String,
}

/// A journal's records ([`Said`]); a line that is no record is skipped.
#[must_use]
pub fn journal_of(text: &str) -> Vec<Said> {
    text.lines()
        .filter_map(|l| {
            let r = J::parse(l).ok()?;
            Some(Said {
                t_ms: millis(r.get("t")?)?,
                line: r.get("line")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// One row of a tab's decision ledger (`drive/<sid>.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    /// `ts`, ms since the epoch.
    pub t_ms: i64,
    /// `rule_id`.
    pub rule: String,
    /// `decision`: `typed`, `approved`, `escalated`, …
    pub decision: String,
}

/// A decision ledger's rows ([`Decided`]).
#[must_use]
pub fn decisions_of(text: &str) -> Vec<Decided> {
    text.lines()
        .filter_map(|l| {
            let r = J::parse(l).ok()?;
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_string();
            Some(Decided {
                t_ms: millis(r.get("ts")?)?,
                rule: s("rule_id"),
                decision: s("decision"),
            })
        })
        .collect()
}

/// One act a tab's hosted loop crossed its control door for (its tape): when, and its ctl
/// words without the tab's own `@sid`, as [`act_line`] writes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Act {
    /// The act's wall clock, ms since the epoch.
    pub t_ms: i64,
    /// Its words, joined by spaces (`key if-human=0 … esc`, `send … -- /login`).
    pub line: String,
}

/// An act as the evidence keeps it: `<t_ms> <words>`.
#[must_use]
pub fn act_line(t_ms: i64, words: &[String]) -> String {
    let kept: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !w.starts_with('@'))
        .collect();
    format!("{t_ms} {}", kept.join(" "))
}

/// The acts [`act_line`] wrote, one a line.
#[must_use]
pub fn acts_of(text: &str) -> Vec<Act> {
    text.lines()
        .filter_map(|l| {
            let (t, line) = l.split_once(' ')?;
            Some(Act {
                t_ms: t.parse().ok()?,
                line: line.to_string(),
            })
        })
        .collect()
}

/// One row of the accounts' ledger (`accounts/ledger.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Booked {
    /// `account`.
    pub account: String,
    /// `kind`: `auth`, `login-ended`, `sign-in`, …
    pub kind: String,
    /// `why`.
    pub why: String,
}

/// The accounts' ledger's rows ([`Booked`]).
#[must_use]
pub fn ledger_of(text: &str) -> Vec<Booked> {
    text.lines()
        .filter_map(|l| {
            let r = J::parse(l).ok()?;
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_string();
            Some(Booked {
                account: s("account"),
                kind: s("kind"),
                why: s("why"),
            })
        })
        .collect()
}

/// One row of the window's `messages.log`: what happened to a message (`posted`, `retired`),
/// when, and its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// `kind`.
    pub kind: String,
    /// `t`, ms since the epoch.
    pub t_ms: i64,
    /// `title`.
    pub title: String,
}

/// `messages.log`'s rows ([`Message`]).
#[must_use]
pub fn messages_of(text: &str) -> Vec<Message> {
    text.lines()
        .filter_map(|l| {
            let field = |k: &str| {
                l.split('\t')
                    .find_map(|f| f.strip_prefix(k)?.strip_prefix('='))
                    .map(str::to_string)
            };
            Some(Message {
                kind: field("kind")?,
                t_ms: field("t")?.parse().ok()?,
                title: field("title").unwrap_or_default(),
            })
        })
        .collect()
}

/// Whether one of `messages` went `kind` within `from..=to`, titled as the notice `title` is.
fn told(messages: &[Message], kind: &str, title: &str, (from, to): (i64, i64)) -> bool {
    messages
        .iter()
        .any(|m| m.kind == kind && (from..=to).contains(&m.t_ms) && m.title.starts_with(title))
}

/// The times, ms since the epoch, of the lines of aterm.log that hold `needle` (each line opens
/// with its time, seconds with a fraction); a line whose time does not read is skipped.
#[must_use]
pub fn log_times(aterm_log: &str, needle: &str) -> Vec<i64> {
    aterm_log
        .lines()
        .filter(|l| l.contains(needle))
        .filter_map(|l| {
            let secs: f64 = l.split_whitespace().next()?.parse().ok()?;
            Some((secs * 1000.0).round() as i64)
        })
        .collect()
}

/// The host's looks at the roster that found its bytes changed ([`ROSTER_LOOK`]), ms since the
/// epoch.
#[must_use]
pub fn looks_of(aterm_log: &str) -> Vec<i64> {
    log_times(aterm_log, ROSTER_LOOK)
}

/// A JSON time as ms since the epoch: an integer of ms, or seconds with a fraction.
fn millis(v: &J) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    v.as_f64().map(|f| (f * 1000.0) as i64)
}

// ---------------------------------------------------------------------------------------------
// The judges.

/// A judge's verdict and the one line of evidence the table prints beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    /// The verdict.
    pub verdict: Verdict,
    /// What it rests on, or what failed it.
    pub evidence: String,
}

impl Judged {
    fn of(missing: &[String], held: String) -> Self {
        if missing.is_empty() {
            Self {
                verdict: Verdict::Ok,
                evidence: held,
            }
        } else {
            Self {
                verdict: Verdict::Fail,
                evidence: missing.join("; "),
            }
        }
    }

    fn not_reached(why: &str) -> Self {
        Self {
            verdict: Verdict::Fail,
            evidence: format!("not reached: {why}"),
        }
    }
}

/// Whether `line` names `label` as a word of its own.
#[must_use]
pub fn names(line: &str, label: &str) -> bool {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c)))
        .any(|w| w == label)
}

/// `line` cut to `n` characters for the table.
fn clip(line: &str, n: usize) -> String {
    let mut out: String = line.chars().take(n).collect();
    if line.chars().count() > n {
        out.push('…');
    }
    out
}

/// Whether the act types into the tab (`key`, `send`, `paste`, `turn`), as a badge, a task
/// mark or a meta write does not.
#[must_use]
pub fn is_input(line: &str) -> bool {
    matches!(
        line.split_whitespace().next(),
        Some("key" | "send" | "paste" | "turn")
    )
}

/// The acts of `acts` that type into the tab ([`is_input`]).
fn inputs(acts: &[Act]) -> Vec<Act> {
    acts.iter().filter(|a| is_input(&a.line)).cloned().collect()
}

/// Whether the act is the loop's fenced `/login`, typed only into an empty composer.
#[must_use]
pub fn is_login_send(line: &str) -> bool {
    line.starts_with("send ") && line.contains(" if=") && line.ends_with(" -- /login")
}

/// Whether the act is the loop's Enter, fenced on the composer row that holds `/login`.
#[must_use]
pub fn is_login_enter(line: &str) -> bool {
    line.starts_with("key ")
        && line.ends_with(" enter")
        && line
            .split_whitespace()
            .any(|w| w.starts_with("if=") && w.contains("/login"))
}

/// Whether the act is an Esc fenced on the person (`if-human=`): the loop's back-out.
#[must_use]
pub fn is_fenced_esc(line: &str) -> bool {
    line.starts_with("key ")
        && line.ends_with(" esc")
        && line.split_whitespace().any(|w| w.starts_with("if-human="))
}

fn within<T>(items: &[T], at: impl Fn(&T) -> i64, from: i64, to: i64) -> Vec<&T> {
    items
        .iter()
        .filter(|i| (from..=to).contains(&at(i)))
        .collect()
}

fn booked(ledger: &[Booked], label: &str, kind: &str, why: impl Fn(&str) -> bool) -> bool {
    ledger
        .iter()
        .any(|b| b.account == label && b.kind == kind && why(&b.why))
}

/// The accounts' ledger for `label`'s lost login: its auth struck signed out, its login
/// ended, and an automatic sign-in that found no route (`no-route`), never a sign-in run.
fn ledger_misses(ledger: &[Booked], label: &str) -> Vec<String> {
    let mut missing = Vec::new();
    if !booked(ledger, label, "auth", |w| w == "signed-out") {
        missing.push(format!(
            "no `auth` signed-out row for {label} in the accounts' ledger"
        ));
    }
    if !booked(ledger, label, "login-ended", |_| true) {
        missing.push(format!("no `login-ended` row for {label}"));
    }
    if !booked(ledger, label, "sign-in", |w| w.contains("no-route")) {
        missing.push(format!("no `sign-in` row for {label} saying no-route"));
    }
    missing
}

/// S1: tab A's lost login, `label`'s row's. The tab waits on aterm's own sign-in of the row
/// (a `WAITING` record naming `label`), types nothing (no `TYPED`, no input on its tape), tells
/// nobody (no `EXCEPTION` or `ESCALATED`), and the accounts' ledger books the login ended and
/// the sign-in's no-route.
#[must_use]
pub fn judge_s1(journal: &[Said], acts: &[Act], ledger: &[Booked], label: &str) -> Judged {
    let acts = inputs(acts);
    let mut missing = Vec::new();
    let waiting = journal
        .iter()
        .find(|s| s.line.starts_with("WAITING ") && names(&s.line, label));
    if waiting.is_none() {
        missing.push(format!("no WAITING record naming {label}"));
    }
    if let Some(t) = journal.iter().find(|s| s.line.starts_with("TYPED ")) {
        missing.push(format!("typed: `{}`", clip(&t.line, 80)));
    }
    if let Some(a) = acts.first() {
        missing.push(format!(
            "{} input(s), the first `{}`",
            acts.len(),
            clip(&a.line, 80)
        ));
    }
    if let Some(t) = journal
        .iter()
        .find(|s| s.line.starts_with("EXCEPTION ") || s.line.starts_with("ESCALATED "))
    {
        missing.push(format!("told: `{}`", clip(&t.line, 80)));
    }
    missing.extend(ledger_misses(ledger, label));
    Judged::of(
        &missing,
        format!(
            "`{}`; nothing typed, no input, nobody told; ledger {label}: signed-out, login-ended, sign-in no-route",
            clip(waiting.map_or("", |s| s.line.as_str()), 90)
        ),
    )
}

/// S2: tab B's lost login, no row's, before S3's mark (`to`). Its tape holds one fenced
/// `/login` and one Enter fenced on the composer row holding it, and no other input (no key in
/// the picker); the journal holds one `TYPED` of [`RULE_LOGIN`] and the login exception told
/// with the [`LOGIN_CHOOSE`] notice; the window posted that notice.
#[must_use]
pub fn judge_s2(journal: &[Said], acts: &[Act], messages: &[Message], to: i64) -> Judged {
    let acts = inputs(acts);
    let mut missing = Vec::new();
    let before = within(&acts, |a| a.t_ms, i64::MIN, to);
    let sends = before.iter().filter(|a| is_login_send(&a.line)).count();
    let enters = before.iter().filter(|a| is_login_enter(&a.line)).count();
    if sends != 1 || enters != 1 {
        missing.push(format!(
            "{sends} fenced /login and {enters} fenced Enter on its row (want one each)"
        ));
    }
    if let Some(other) = before
        .iter()
        .find(|a| !is_login_send(&a.line) && !is_login_enter(&a.line))
    {
        missing.push(format!("another input: `{}`", clip(&other.line, 80)));
    }
    let typed_rule = format!(" rule={RULE_LOGIN} ");
    let typed: Vec<&Said> = within(journal, |s| s.t_ms, i64::MIN, to)
        .into_iter()
        .filter(|s| s.line.starts_with("TYPED ") && s.line.contains(&typed_rule))
        .collect();
    if typed.len() != 1 {
        missing.push(format!(
            "{} TYPED {RULE_LOGIN} records (want one)",
            typed.len()
        ));
    }
    let notice = format!(" notice={}", LOGIN_CHOOSE.id);
    let exception = journal.iter().find(|s| {
        s.t_ms <= to
            && s.line.starts_with("EXCEPTION ")
            && s.line.contains(" kind=login ")
            && s.line.contains(&notice)
    });
    if exception.is_none() {
        missing.push(format!("no EXCEPTION kind=login{notice}"));
    }
    if !told(messages, "posted", LOGIN_CHOOSE.title, (i64::MIN, to)) {
        missing.push(format!(
            "no message posted titled `{}…`",
            LOGIN_CHOOSE.title
        ));
    }
    Judged::of(
        &missing,
        format!(
            "`{}`; inputs: fenced /login, fenced Enter, nothing else; `{}`",
            clip(typed.first().map_or("", |s| s.line.as_str()), 60),
            clip(exception.map_or("", |s| s.line.as_str()), 70)
        ),
    )
}

/// S3: the row `label` added for tab B's login at `from`; `next` S3b's start. Between them one
/// input, an Esc fenced on the person, and one approved [`RULE_LOGIN_BACK_OUT`] decision; that
/// Esc before the host's first look at the roster at or after `from` (`looks`, [`looks_of`]),
/// so the add's own wake brought the news, not the host's look; after the Esc, before `next`,
/// the login exception `CLEARED` naming the back-out ([`LOGIN_BACKED_OUT`]), the
/// [`LOGIN_CHOOSE`] message withdrawn and a `WAITING` record naming `label`; no `/login` typed
/// again; and the accounts' ledger books `label`'s no-route sign-in.
#[must_use]
pub fn judge_s3(
    (journal, decisions, acts): (&[Said], &[Decided], &[Act]),
    (ledger, messages, looks): (&[Booked], &[Message], &[i64]),
    label: &str,
    (from, next): (i64, i64),
) -> Judged {
    let acts = inputs(acts);
    let mut missing = Vec::new();
    let between = within(&acts, |a| a.t_ms, from, next);
    let esc = between.iter().find(|a| is_fenced_esc(&a.line));
    match (esc, between.len()) {
        (Some(_), 1) => {}
        (None, _) => missing.push("no fenced Esc after the add".into()),
        (Some(_), n) => missing.push(format!(
            "{n} inputs between the add and S3b (want the one Esc)"
        )),
    }
    let approved = within(decisions, |d| d.t_ms, from, next)
        .into_iter()
        .filter(|d| d.rule == RULE_LOGIN_BACK_OUT && d.decision == "approved")
        .count();
    if approved != 1 {
        missing.push(format!(
            "{approved} approved {RULE_LOGIN_BACK_OUT} rows (want one)"
        ));
    }
    let look = looks.iter().copied().filter(|&t| t >= from).min();
    let secs = |ms: i64| ms as f64 / 1000.0;
    match (esc, look) {
        (Some(e), Some(l)) if l <= e.t_ms => missing.push(format!(
            "the Esc came {:.1} s after the host's own look at the roster ({:.1} s after the add): \
             the add's wake did not bring the news",
            secs(e.t_ms - l),
            secs(l - from)
        )),
        (Some(_), None) => missing.push(format!(
            "no look of the host's at the roster after the add (aterm.log's `{ROSTER_LOOK}`), \
             so the add's wake is not told from it"
        )),
        _ => {}
    }
    let after = esc.map_or(next, |a| a.t_ms);
    let later = within(journal, |s| s.t_ms, after, next);
    let cleared = later.iter().find(|s| {
        s.line.starts_with("CLEARED ")
            && s.line.contains(" exception=login ")
            && s.line.ends_with(LOGIN_BACKED_OUT)
    });
    if cleared.is_none() {
        missing.push("no CLEARED exception=login naming the back-out after the Esc".into());
    }
    let waiting = later
        .iter()
        .find(|s| s.line.starts_with("WAITING ") && names(&s.line, label));
    if waiting.is_none() {
        missing.push(format!("no WAITING naming {label} after the Esc"));
    }
    let typed_rule = format!(" rule={RULE_LOGIN} ");
    if within(journal, |s| s.t_ms, from, next)
        .iter()
        .any(|s| s.line.starts_with("TYPED ") && s.line.contains(&typed_rule))
    {
        missing.push("/login typed again".into());
    }
    if !booked(ledger, label, "sign-in", |w| w.contains("no-route")) {
        missing.push(format!("no `sign-in` row for {label} saying no-route"));
    }
    if !told(messages, "retired", LOGIN_CHOOSE.title, (after, next)) {
        missing.push(format!(
            "the `{}` message was not withdrawn",
            LOGIN_CHOOSE.id
        ));
    }
    Judged::of(
        &missing,
        format!(
            "Esc {:.1} s after the add, {:.1} s before the host's own look at the roster; \
             {RULE_LOGIN_BACK_OUT} approved, the notice withdrawn; `{}`; `{}`",
            esc.map_or(0.0, |a| secs(a.t_ms - from)),
            match (esc, look) {
                (Some(e), Some(l)) => secs(l - e.t_ms),
                _ => 0.0,
            },
            clip(cleared.map_or("", |s| s.line.as_str()), 60),
            clip(waiting.map_or("", |s| s.line.as_str()), 60)
        ),
    )
}

/// S3b: a person's own `/login` in tab B, entered at `from`, its picker watched until `to`
/// (`watched` long from when it was drawn, which must be at least the supervisor's `grace`,
/// [`s3b_grace`]). No input at all, nothing approved and nothing typed in the window, no
/// [`LOGIN_CHOOSE`] notice posted for it, and a decision that escalates it to the person.
#[must_use]
pub fn judge_s3b(
    (journal, decisions, acts): (&[Said], &[Decided], &[Act]),
    messages: &[Message],
    (from, to): (i64, i64),
    (watched, grace): (Duration, Duration),
) -> Judged {
    let acts = inputs(acts);
    let mut missing = Vec::new();
    let in_window = within(&acts, |a| a.t_ms, from, to);
    if let Some(a) = in_window.first() {
        missing.push(format!(
            "{} input(s) at the person's picker, the first `{}`",
            in_window.len(),
            clip(&a.line, 80)
        ));
    }
    let rows = within(decisions, |d| d.t_ms, from, to);
    if let Some(d) = rows.iter().find(|d| d.decision == "approved") {
        missing.push(format!("approved: {}", d.rule));
    }
    if !rows.iter().any(|d| d.decision == "escalated") {
        missing.push("no decision escalated it to the person".into());
    }
    if let Some(t) = within(journal, |s| s.t_ms, from, to)
        .iter()
        .find(|s| s.line.starts_with("TYPED "))
    {
        missing.push(format!("typed: `{}`", clip(&t.line, 80)));
    }
    if told(messages, "posted", LOGIN_CHOOSE.title, (from, to)) {
        missing.push(format!(
            "the `{}` notice was posted for it",
            LOGIN_CHOOSE.id
        ));
    }
    if watched < grace {
        missing.push(format!(
            "watched {} s, under the supervisor's {} s grace",
            watched.as_secs(),
            grace.as_secs()
        ));
    }
    Judged::of(
        &missing,
        format!(
            "no key, nothing approved for {} s (the supervisor's grace {} s); escalated to the person",
            watched.as_secs(),
            grace.as_secs()
        ),
    )
}

/// The process watcher: no `auth login` and no browser or `open` launch of the lab was seen
/// (`sightings`); unproven when its control (a jailed process carrying `auth login`) was not.
#[must_use]
pub fn judge_procs(sightings: &[String], control: bool, looks: u64) -> Judged {
    if let Some(s) = sightings.first() {
        return Judged {
            verdict: Verdict::Fail,
            evidence: format!(
                "{} sighting(s), the first `{}`",
                sightings.len(),
                clip(s, 90)
            ),
        };
    }
    Judged {
        verdict: if control {
            Verdict::Ok
        } else {
            Verdict::Unproven
        },
        evidence: format!(
            "no `auth login`, browser or open process of the lab in {looks} looks{}",
            if control {
                " (its control, a jailed `auth login`, was seen)"
            } else {
                "; its control was NOT seen"
            }
        ),
    }
}

/// The egress witness: no `EXTERNAL` socket in either log; unproven when it logged nothing.
#[must_use]
pub fn judge_egress(sockets_log: &str, egress_log: &str) -> Judged {
    let external = egress_log
        .lines()
        .chain(sockets_log.lines())
        .filter(|l| l.contains("EXTERNAL"))
        .count();
    let sampled = sockets_log.lines().count();
    Judged {
        verdict: if external > 0 {
            Verdict::Fail
        } else if sampled > 0 {
            Verdict::Ok
        } else {
            Verdict::Unproven
        },
        evidence: format!("{external} EXTERNAL endpoints; {sampled} socket rows sampled"),
    }
}

/// The keychain items the owner's own Claude Code login is kept under, un-suffixed: a launch
/// whose `CLAUDE_SECURESTORAGE_CONFIG_DIR` took asks for its login directory's suffixed ones.
const OWNER_ITEMS: [&str; 2] = ["Claude Code", "Claude Code-credentials"];

/// The keychain tripwire (`security.log`, the lab's `security` stand-in's record of each item it
/// was asked for): Fail if Claude Code asked for one of the owner's un-suffixed items
/// ([`OWNER_ITEMS`]); else Ok if it asked for any (scratch-suffixed ones alone); else Unproven,
/// the tripwire never reached.
#[must_use]
pub fn judge_keychain(security_log: &str) -> Judged {
    let services: Vec<&str> = security_log
        .lines()
        .filter_map(|l| {
            let (_, rest) = l.split_once("service=\"")?;
            rest.split_once('"').map(|(service, _)| service)
        })
        .collect();
    let owners: Vec<&str> = services
        .iter()
        .copied()
        .filter(|s| OWNER_ITEMS.contains(s))
        .collect();
    let distinct: std::collections::BTreeSet<&str> = services.iter().copied().collect();
    Judged {
        verdict: if !owners.is_empty() {
            Verdict::Fail
        } else if services.is_empty() {
            Verdict::Unproven
        } else {
            Verdict::Ok
        },
        evidence: if let Some(o) = owners.first() {
            format!(
                "{} lookup(s) of the owner's own items, the first `{o}`",
                owners.len()
            )
        } else if services.is_empty() {
            "the keychain tripwire was never asked for an item".into()
        } else {
            format!(
                "{} lookups, every one a scratch-suffixed item ({})",
                services.len(),
                clip(&distinct.into_iter().collect::<Vec<_>>().join(", "), 90)
            )
        },
    }
}

/// The owner's data: the jail's profile `jail_sb` denies every write under each of the owner's
/// `homes` (their accounts store and `~/.config/aterm` among them).
#[must_use]
pub fn judge_owner(jail_sb: &str, homes: &[String]) -> Judged {
    let open: Vec<&String> = homes
        .iter()
        .filter(|h| !jail_sb.contains(&format!("(deny file-write* (subpath \"{h}\"))")))
        .collect();
    Judged {
        verdict: if open.is_empty() && !homes.is_empty() {
            Verdict::Ok
        } else {
            Verdict::Fail
        },
        evidence: if open.is_empty() {
            format!(
                "the jail denies every write under the owner's home ({} guarded)",
                homes.len()
            )
        } else {
            format!(
                "the jail does not deny writes under {}",
                open.iter()
                    .map(|h| h.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    }
}

/// S3b's grace: the seconds the supervisor keeps its hands off a session after a person's keys
/// (`human_grace_s` of the lab's own `aterm.toml`), and the turn end's own pause before a
/// dialog of a `/login` it typed is pressed again ([`TurnEndTiming::take_within`]).
#[must_use]
pub fn s3b_grace(cfg: &SupervisorConfig) -> Duration {
    Duration::from_secs(u64::from(cfg.human_grace_s)) + TurnEndTiming::default().take_within
}

/// How long S3's add waits, at `now`, to land at most [`S3_ADD_AFTER_LOOK`] after one of the
/// host's looks at the roster: those come every [`HOST_LOOK_EVERY`] from the latest one aterm.log
/// shows (the activation wake's arming, [`WAKE_ARMED`], or a look that logged [`ROSTER_LOOK`]),
/// and a wait lands [`LOOK_SETTLE`] past the next. `None`: at once, the last look near enough,
/// or none logged.
#[must_use]
pub fn add_wait(aterm_log: &str, now: i64) -> Option<Duration> {
    let last = log_times(aterm_log, WAKE_ARMED)
        .into_iter()
        .chain(looks_of(aterm_log))
        .max()?;
    let every = i64::try_from(HOST_LOOK_EVERY.as_millis()).unwrap_or(i64::MAX);
    let near = i64::try_from(S3_ADD_AFTER_LOOK.as_millis()).unwrap_or(0);
    let phase = (now - last).rem_euclid(every);
    (phase > near)
        .then(|| Duration::from_millis(u64::try_from(every - phase).unwrap_or(0)) + LOOK_SETTLE)
}

// ---------------------------------------------------------------------------------------------
// The process watcher.

/// What the watcher saw.
#[derive(Debug, Default, Clone)]
struct Seen {
    sightings: Vec<String>,
    control: bool,
    looks: u64,
}

/// Looks at every process every 200 ms for the run's whole length: a lab process (one naming
/// the run's directory, a descendant of its instance, or one in its jail; the instance itself
/// apart) carrying `auth login`, or one that is a browser or `open`, is a sighting.
struct Watcher {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Seen>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// What a process's command line is suspected of: `auth login`, or a browser or `open` launch.
fn suspect(command: &str) -> Option<&'static str> {
    let argv0 = command.split_whitespace().next().unwrap_or("");
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    if command.contains(" auth login") {
        Some("auth login")
    } else if base == "open" || base == "osascript" || command.contains(".app/Contents/MacOS/") {
        Some("open")
    } else {
        None
    }
}

impl Watcher {
    fn start(run: &Run) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (instance, mark) = (run.instance_pid, run.mark.clone());
        let dir = run.run.to_string_lossy().into_owned();
        let (s, k) = (Arc::clone(&stop), Arc::clone(&seen));
        let handle = std::thread::spawn(move || {
            let mut said = std::collections::BTreeSet::new();
            while !s.load(Ordering::SeqCst) {
                let rows = ps_rows();
                let table: Vec<(i32, i32)> = rows.iter().map(|(p, pp, _)| (*p, *pp)).collect();
                let family = crate::procs::descendants_in(&table, instance);
                let mut look = k.lock().unwrap_or_else(PoisonError::into_inner);
                look.looks += 1;
                for (pid, _, command) in &rows {
                    // The instance itself is no launch of a browser, wherever its binary lies.
                    let Some(what) = suspect(command).filter(|_| *pid != instance) else {
                        continue;
                    };
                    let jailed = || crate::sys::jail_pids(&mark).is_ok_and(|v| v.contains(pid));
                    let ours = command.contains(&dir) || family.contains(pid) || jailed();
                    if !ours {
                        continue;
                    }
                    if command.contains(PROC_CONTROL) {
                        look.control = true;
                    } else if !command.contains(crate::recorder::CONTROL_URL)
                        && said.insert((*pid, command.clone()))
                    {
                        look.sightings.push(format!("{what}: pid {pid} {command}"));
                    }
                }
                drop(look);
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        Watcher {
            stop,
            seen,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> Seen {
        self.halt();
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.halt();
    }
}

/// `ps -axww -o pid=,ppid=,command=`: every process, its parent and its whole command line.
fn ps_rows() -> Vec<(i32, i32, String)> {
    let Ok(out) = aterm_uds::spawnfd::command("/bin/ps")
        .args(["-axww", "-o", "pid=,ppid=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (pid, rest) = l.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (ppid, command) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            Some((
                pid.parse().ok()?,
                ppid.parse().ok()?,
                command.trim().to_string(),
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The world and the tabs.

/// The paths of the lab's dead-login world.
struct World {
    /// The roster the instance reads (`cfg/aterm/accounts.toml`).
    roster: PathBuf,
    /// The lab's `aterm.toml`.
    config: PathBuf,
    /// The instance's harness state (`state/harness/claude-harness`).
    harness: PathBuf,
    /// S1's login directory: the scratch HOME's `.claude`, `lab-a`'s row's.
    login_a: PathBuf,
    /// S2's: the accounts store's own directory for `lab-b`, which no row names until S3.
    login_b: PathBuf,
}

impl World {
    fn of(run: &Run) -> Result<World, Fail> {
        let harness = run.state().join("harness").join("claude-harness");
        let login_b = Store::at(&harness)
            .login_dir(LABEL_B)
            .ok_or_else(|| Fail::die("lab-b names no login directory"))?;
        Ok(World {
            roster: run.w.join("cfg/aterm/accounts.toml"),
            config: run.w.join("cfg/aterm/aterm.toml"),
            harness,
            login_a: run.w.join("home/.claude"),
            login_b,
        })
    }
}

fn die(what: &str, e: impl std::fmt::Display) -> Fail {
    Fail::die(format!("claude-login-proof: {what}: {e}"))
}

/// Lay the default route's dead-login world in the lab's scratch, nowhere else: settings with
/// no `apiKeyHelper` and no route of the stub's, a HOME `.claude.json` with no `oauthAccount`,
/// a dead `.credentials.json` in each login directory, and the roster naming `lab-a` alone.
fn lay_world(run: &Run, world: &World) -> Result<(), Fail> {
    for p in [&world.roster, &world.config, &world.login_a, &world.login_b] {
        if !p.starts_with(&run.w) {
            return Err(die("refusing to write outside the scratch", p.display()));
        }
    }
    let home = run.w.join("home");
    for mailbox in [
        home.join(".secrets").join(MAILBOX_TOML_FILE),
        home.join(".secrets").join(SECRETS_MAILBOX_TOML_FILE),
        run.w.join("cfg/aterm").join(MAILBOX_TOML_FILE),
        run.w.join("cfg/aterm/secrets/state.toml"),
    ] {
        if mailbox.exists() {
            return Err(die(
                "a mailbox is configured in the scratch, so a sign-in could reach one",
                mailbox.display(),
            ));
        }
    }
    let mut env = J::Obj(Vec::new());
    for k in [
        "ANTHROPIC_BASE_URL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        env.set(k, J::Null);
    }
    env.set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", J::s("1"));
    crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null), ("env", env)]))?;
    let config = crate::verbs::read_scratch(&run.w.join("home/.claude/.claude.json"))?;
    let mut config = J::parse(&config).map_err(|e| die("the scratch .claude.json", e))?;
    config.remove("oauthAccount");
    let to = home.join(".claude.json");
    atomic_write(&to, config.dump(crate::pyjson::COMPACT).as_bytes())
        .map_err(|e| die(&to.display().to_string(), e))?;
    for dir in [&world.login_a, &world.login_b] {
        fs::create_dir_all(dir).map_err(|e| die(&dir.display().to_string(), e))?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        let creds = dir.join(".credentials.json");
        crate::sys::put(
            &creds,
            0o600,
            &crate::sys::PutSrc::Bytes(DEAD_LOGIN.as_bytes()),
        )
        .map_err(|e| die(&creds.display().to_string(), e))?;
    }
    let roster = format!(
        "[[account]]\nlabel = \"{LABEL_A}\"\nclass = \"dedicated\"\nemail = \"nobody@example.test\"\ndir = \"{}\"\n",
        world.login_a.display()
    );
    Roster::parse(&roster).map_err(|e| die("the roster the lab writes", e))?;
    atomic_write(&world.roster, roster.as_bytes())
        .map_err(|e| die(&world.roster.display().to_string(), e))
}

/// The tab's launcher: the installed Claude Code by path on its default route, its stored
/// login `login`, every `open` and `$BROWSER` the lab's recorder.
fn launcher(run: &Run, login: &Path) -> String {
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    let w = &run.w;
    format!(
        "#!/bin/sh\nclear\nexec env -u CLAUDE_CONFIG_DIR -u ANTHROPIC_BASE_URL -u HTTP_PROXY -u HTTPS_PROXY \
         -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \\\n  PATH=\"{}:$PATH\" {LOGIN_ENV}={} \
         DISABLE_AUTOUPDATER=1 \\\n  LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} \\\n  {}\n",
        run.run.join("shim").display(),
        q(login),
        q(&w.join("security.log")),
        q(&w.join("open.log")),
        q(&run.run.join("shim/open")),
        shell_quote(&run.claude_bin)
    )
}

/// `@sid <args>` against the lab.
fn on(door: &Door<'_>, sid: &str, args: &[&str]) -> Result<crate::door::Reply, Fail> {
    let at = format!("@{sid}");
    let mut v = vec![at.as_str()];
    v.extend_from_slice(args);
    Ok(door.ctl(&v)?)
}

/// Whether a fenced input went in: `OK`, not `OK skipped` (its fence's row did not stand).
fn went(r: &crate::door::Reply) -> bool {
    r.code == 0 && r.stdout.starts_with("OK") && !r.stdout.contains("skipped")
}

/// Supervise tab `sid` under cruise with a task, launch Claude Code in it on `login`, wait for
/// its empty composer and type `prompt` ([`first_prompt`]).
fn start_tab(
    run: &Run,
    door: &Door<'_>,
    sid: &str,
    login: &Path,
    prompt: &str,
) -> Result<(), Fail> {
    for args in [
        &["meta", "set", "autopilot", "cruise"][..],
        &["task", "mark", "working", "from=ctl"],
    ] {
        let r = on(door, sid, args)?;
        if r.code != 0 {
            return Err(die(&args.join(" "), r.stderr.trim()));
        }
    }
    let text = launcher(run, login);
    crate::verbs::record_launch(run, sid, &text)?;
    let script = crate::verbs::launch_path(run, sid);
    let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    on(door, sid, &["send", &line])?;
    on(door, sid, &["key", "enter"])?;
    if let Some(why) = crate::forever::empty_composer(door, sid, "claude")?.why("Claude Code") {
        return Err(die(&format!("tab {sid}"), why));
    }
    if let Err(why) = first_prompt(run, door, sid, prompt)? {
        return Err(die(
            &format!("tab {sid}"),
            format!("the composer did not take the prompt: {why}"),
        ));
    }
    Ok(())
}

/// Type `prompt` into tab `sid`'s empty composer and press its Enter, each once and fenced on the
/// composer's own row (the empty one, then the one holding `prompt`), so neither lands anywhere
/// else (in a `/login` picker Claude Code drew, say): a fence whose row does not stand skips its
/// key, and nothing is pressed again. `Err`: why the prompt was not sent.
fn first_prompt(
    run: &Run,
    door: &Door<'_>,
    sid: &str,
    prompt: &str,
) -> Result<Result<(), String>, Fail> {
    let row = || {
        let text = on(door, sid, &["text"])
            .map(|r| r.stdout)
            .unwrap_or_default();
        crate::screen::composer_row("claude", &text)
    };
    let sent = on(door, sid, &["send", EMPTY_COMPOSER, "--", prompt])?;
    if !went(&sent) {
        return Ok(Err(format!("not typed ({})", sent.stdout.trim())));
    }
    let stood = hang_wait(run, door, sid, "the prompt in the composer", || {
        (row().as_deref() == Some(prompt)).then_some(())
    })?;
    if stood.is_err() {
        return Ok(stood);
    }
    // A breath between the typing and its Enter, so the Enter is no part of a paste.
    stop::sleep(Duration::from_millis(300))?;
    let fence = format!("if={}", row_guard(&format!("❯\u{a0}{prompt}")));
    let entered = on(door, sid, &["key", &fence, "enter"])?;
    if !went(&entered) {
        return Ok(Err(format!(
            "its Enter was not pressed ({})",
            entered.stdout.trim()
        )));
    }
    hang_wait(run, door, sid, "the prompt sent", || {
        (row().as_deref() != Some(prompt)).then_some(())
    })
}

/// The live negative control's S3 ([`scenarios`]' `by_hand`): [`LABEL_B`]'s row, as `accounts
/// add` would list it, written into the roster by the lab itself, so no wake marker tells the
/// host of it and only the host's own look at the roster can.
fn row_by_hand(world: &World) -> Result<(), Fail> {
    let row = Row {
        label: LABEL_B.to_string(),
        dir: Some(world.login_b.to_string_lossy().into_owned()),
        email: Some(EMAIL_B.to_string()),
        ..Row::default()
    };
    let text = accounts::append_rows(&scratch_text(&world.roster), &[row])
        .map_err(|e| die("the row written by hand", e))?;
    atomic_write(&world.roster, text.as_bytes())
        .map_err(|e| die(&world.roster.display().to_string(), e))
}

fn journal_path(run: &Run, sid: &str) -> PathBuf {
    run.state()
        .join("drive")
        .join(format!("{sid}.journal.jsonl"))
}

/// The instance's own log (`state/logs/aterm.log`).
fn aterm_log(run: &Run) -> PathBuf {
    run.state().join("logs").join("aterm.log")
}

fn read_journal(run: &Run, sid: &str) -> Vec<Said> {
    journal_of(&scratch_text(&journal_path(run, sid)))
}

fn read_decisions(run: &Run, sid: &str) -> Vec<Decided> {
    decisions_of(&scratch_text(
        &run.state().join("drive").join(format!("{sid}.jsonl")),
    ))
}

/// Wait until `found` holds, watching tab `sid`'s screen and journal and the instance's
/// aterm.log: a minute in which none of them moved ([`HANG`]), or [`WAIT_CAP`] in all, is a
/// hang, said as `Err`.
fn hang_wait<T>(
    run: &Run,
    door: &Door<'_>,
    sid: &str,
    what: &str,
    mut found: impl FnMut() -> Option<T>,
) -> Result<Result<T, String>, Fail> {
    let start = Instant::now();
    let (mut moved, mut last) = (Instant::now(), (String::new(), 0usize, 0usize));
    let log = aterm_log(run);
    loop {
        if let Some(t) = found() {
            return Ok(Ok(t));
        }
        let now = (
            on(door, sid, &["text"])
                .map(|r| r.stdout)
                .unwrap_or_default(),
            read_journal(run, sid).len(),
            scratch_text(&log).len(),
        );
        if now != last {
            (moved, last) = (Instant::now(), now);
        }
        if moved.elapsed() >= HANG || start.elapsed() >= WAIT_CAP {
            return Ok(Err(format!(
                "{what}: not seen ({} s, the last {} s with nothing moving)",
                start.elapsed().as_secs(),
                moved.elapsed().as_secs()
            )));
        }
        stop::sleep(Duration::from_millis(500))?;
    }
}

/// The marks the scenarios left for the judges, ms since the epoch.
#[derive(Debug, Default)]
struct Marks {
    sid_a: String,
    sid_b: String,
    /// S3's start: the add (or, under `--roster-by-hand`, the row written by hand).
    s3: Option<i64>,
    /// S3b's start: the person's Enter under their `/login`.
    s3b: Option<i64>,
    /// S3b's end.
    s3b_end: Option<i64>,
    /// How long S3b watched the person's picker, from when it was drawn.
    watched: Duration,
    /// Why the scenarios stopped short, where they did.
    blocked: Option<String>,
    /// `accounts add`'s reply (or what the hand wrote).
    added: String,
}

/// Run S1, S2, S3 and S3b on the lab; a scenario that cannot go on says why in `blocked`.
/// `by_hand`: S3's row is written into the roster by hand ([`row_by_hand`]), not by `accounts
/// add`, the live negative control.
fn scenarios(
    run: &Run,
    world: &World,
    by_hand: bool,
    grace: Duration,
    m: &mut Marks,
) -> Result<(), Fail> {
    let door = Door::new(run)?;
    m.sid_a = crate::verbs::sid(&door);
    start_tab(run, &door, &m.sid_a, &world.login_a, PROMPT_A)?;
    let spawned = door.ctl(&["spawn", &format!("cwd={}", run.proj)])?;
    m.sid_b = spawned
        .stdout
        .split_whitespace()
        .nth(1)
        .filter(|_| spawned.stdout.starts_with("OK "))
        .ok_or_else(|| die("spawn", spawned.stdout.trim()))?
        .to_string();
    start_tab(run, &door, &m.sid_b, &world.login_b, PROMPT_B)?;
    let (a, b) = (m.sid_a.clone(), m.sid_b.clone());
    let s1 = hang_wait(run, &door, &a, "S1's WAITING naming lab-a", || {
        read_journal(run, &a)
            .iter()
            .any(|s| s.line.starts_with("WAITING ") && names(&s.line, LABEL_A))
            .then_some(())
    })?;
    if let Err(why) = s1 {
        println!("  (S1) {why}");
    }
    let notice = format!(" notice={}", LOGIN_CHOOSE.id);
    let told = hang_wait(
        run,
        &door,
        &b,
        "S2's login exception at aterm's picker",
        || {
            read_journal(run, &b)
                .iter()
                .any(|s| s.line.starts_with("EXCEPTION ") && s.line.contains(&notice))
                .then_some(())
        },
    )?;
    if let Err(why) = told {
        m.blocked = Some(why);
        return Ok(());
    }
    let backed = |from: i64| {
        read_decisions(run, &b)
            .iter()
            .find(|d| d.t_ms >= from && d.rule == RULE_LOGIN_BACK_OUT && d.decision == "approved")
            .map(|d| d.t_ms)
    };
    let log = scratch_text(&aterm_log(run));
    if let Some(wait) = add_wait(&log, now_ms()) {
        println!(
            "  (S3) the host looks at the roster every {} s by itself: the add waits {:.0} s, to land \
             just past its next look, so a back-out the add's wake brings is told from one that look brings",
            HOST_LOOK_EVERY.as_secs(),
            wait.as_secs_f64()
        );
        stop::sleep(wait)?;
    } else if log_times(&log, WAKE_ARMED).is_empty() {
        println!(
            "  (S3) aterm.log names no arming of the activation wake (`{WAKE_ARMED}`): the add is \
             not placed after a look of the host's"
        );
    }
    let mark = now_ms();
    m.s3 = Some(mark);
    if by_hand {
        row_by_hand(world)?;
        m.added = "--roster-by-hand: the row written into accounts.toml by hand (no wake)".into();
        println!(
            "  (S3) --roster-by-hand: {LABEL_B}'s row written into the roster by hand, no wake"
        );
    } else {
        let state = world.harness.to_string_lossy().into_owned();
        let config = world.config.to_string_lossy().into_owned();
        let r = door.harness(&[
            "accounts", "add", LABEL_B, "--email", EMAIL_B, "--state", &state, "--config", &config,
        ])?;
        m.added = format!("{}{}", r.stdout, r.stderr)
            .trim()
            .replace('\n', " | ");
        println!(
            "  (S3) `aterm harness accounts add {LABEL_B}` took {:.1} s",
            (now_ms() - mark) as f64 / 1000.0
        );
        if r.code != 0 {
            m.blocked = Some(format!("accounts add failed: {}", m.added));
            return Ok(());
        }
    }
    let listed = Roster::parse(&scratch_text(&world.roster))
        .ok()
        .and_then(|roster| roster.row(LABEL_B).and_then(|row| row.dir.clone()));
    if listed.as_deref() != Some(&*world.login_b.to_string_lossy()) {
        m.blocked = Some(format!(
            "the added row names {listed:?}, not tab B's login {}",
            world.login_b.display()
        ));
        return Ok(());
    }
    let out = hang_wait(run, &door, &b, "aterm's back-out of its picker", || {
        backed(mark)
    })?;
    match out {
        Ok(at) => println!(
            "  (S3) aterm backed out of its picker {:.1} s after the add",
            (at - mark) as f64 / 1000.0
        ),
        Err(why) => {
            m.blocked = Some(format!(
                "no back-out of aterm's picker after the add: {why}"
            ));
            return Ok(());
        }
    }
    let waiting = hang_wait(run, &door, &b, "tab B's WAITING naming lab-b", || {
        read_journal(run, &b)
            .iter()
            .any(|s| s.t_ms >= mark && s.line.starts_with("WAITING ") && names(&s.line, LABEL_B))
            .then_some(())
    })?;
    if let Err(why) = waiting {
        println!("  (S3) {why}");
    }
    if let Some(why) = crate::forever::empty_composer(&door, &b, "claude")?.why("Claude Code") {
        m.blocked = Some(format!("S3b: {why}"));
        return Ok(());
    }
    let sent = on(&door, &b, &["send", EMPTY_COMPOSER, "--", "/login"])?;
    if !went(&sent) {
        m.blocked = Some(format!(
            "S3b: the person's /login was not typed ({})",
            sent.stdout.trim()
        ));
        return Ok(());
    }
    let typed = hang_wait(
        run,
        &door,
        &b,
        "the person's /login in the composer",
        || {
            let text = on(&door, &b, &["text"])
                .map(|r| r.stdout)
                .unwrap_or_default();
            crate::screen::composer_row("claude", &text)
                .is_some_and(|row| row == "/login")
                .then_some(())
        },
    )?;
    if let Err(why) = typed {
        m.blocked = Some(format!("S3b: {why}"));
        return Ok(());
    }
    let pressed = now_ms();
    let entered = on(&door, &b, &["key", LOGIN_COMPOSER, "enter"])?;
    if !went(&entered) {
        m.blocked = Some(format!(
            "S3b: the person's Enter was not pressed ({})",
            entered.stdout.trim()
        ));
        return Ok(());
    }
    let picker = hang_wait(run, &door, &b, "the person's /login picker", || {
        on(&door, &b, &["text"])
            .is_ok_and(|r| r.stdout.contains(PICKER_ROW))
            .then_some(())
    })?;
    if let Err(why) = picker {
        m.blocked = Some(format!("S3b: {why}"));
        return Ok(());
    }
    let from = Instant::now();
    m.s3b = Some(pressed);
    let window = grace + S3B_MARGIN;
    println!(
        "  (S3b) the person's picker is drawn; nothing is pressed for {} s (the supervisor's grace {} s and {} s more)",
        window.as_secs(),
        grace.as_secs(),
        S3B_MARGIN.as_secs()
    );
    while from.elapsed() < window {
        stop::sleep(Duration::from_secs(1))?;
    }
    m.watched = from.elapsed();
    m.s3b_end = Some(now_ms());
    println!("  (S3b) the picker is left open; `down` closes the tab (no key is pressed in it)");
    Ok(())
}

/// The acts on every tape of tab `sid` under `drive`, as [`act_line`]s.
fn tape_acts(drive: &Path, sid: &str) -> String {
    let dir = drive.join("tapes").join(sid);
    let mut tapes: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "atape"))
                .collect()
        })
        .unwrap_or_default();
    tapes.sort();
    let mut out = String::new();
    for p in tapes {
        let Ok(tape) = fs::read(&p)
            .map_err(|e| e.to_string())
            .and_then(|b| show::open(&b, true))
        else {
            continue;
        };
        for item in &tape.items {
            let Item::Effect { stamp, .. } = item else {
                continue;
            };
            for (door, request) in corpus::acts(std::slice::from_ref(item)) {
                if door != doors::CTL {
                    continue;
                }
                let Ok(words) = codec::ctl_args(&request) else {
                    continue;
                };
                out.push_str(&act_line(stamp.as_ref().map_or(0, |s| s.wall_ms), &words));
                out.push('\n');
            }
        }
    }
    out
}

/// Where the judges read a run's evidence: the logs `down` kept, or, under `--keep`, the lab's
/// live scratch and run directory.
struct Evidence {
    /// The harness's `drive` directory: the tabs' journals, decision ledgers and tapes.
    drive: PathBuf,
    /// The window's `messages.log`.
    messages: String,
    /// The instance's `aterm.log`.
    aterm_log: String,
    /// The egress witness's socket samples and its log.
    sockets: String,
    egress: String,
    /// The jail's profile.
    jail_sb: String,
    /// The keychain tripwire's log.
    security: String,
    /// The accounts' ledger, read before `down`.
    ledger: String,
    /// Where the acts read off the tapes are kept as `acts-<sid>.txt` (the kept logs), if anywhere.
    keep: Option<PathBuf>,
}

impl Evidence {
    /// The logs `down` kept in `kept`.
    fn kept(kept: &Path, ledger: String) -> Evidence {
        let text = |f: &str| fs::read_to_string(kept.join(f)).unwrap_or_default();
        Evidence {
            drive: kept.join("drive"),
            messages: text("state-logs/messages.log"),
            aterm_log: text("state-logs/aterm.log"),
            sockets: text("sockets.log"),
            egress: text("egress.log"),
            jail_sb: text("jail.sb"),
            security: text("security.log"),
            ledger,
            keep: Some(kept.to_path_buf()),
        }
    }

    /// The live lab's, read through no link (`--keep`).
    fn live(run: &Run, ledger: String) -> Evidence {
        Evidence {
            drive: run.state().join("drive"),
            messages: scratch_text(&run.state().join("logs").join("messages.log")),
            aterm_log: scratch_text(&aterm_log(run)),
            sockets: scratch_text(&run.run.join("sockets.log")),
            egress: scratch_text(&run.run.join("egress.log")),
            jail_sb: scratch_text(&run.run.join("jail.sb")),
            security: scratch_text(&run.w.join("security.log")),
            ledger,
            keep: None,
        }
    }
}

/// What the run left for the judges besides its evidence's text.
struct Ran<'a> {
    marks: &'a Marks,
    seen: &'a Seen,
    grace: Duration,
    witness: crate::recorder::Witness,
    /// Whether the egress witness was still running at the end.
    witness_live: bool,
}

/// One verdict row per judge, in the table's order.
fn judge_run(ev: &Evidence, ran: &Ran<'_>) -> Vec<(&'static str, Judged)> {
    let (marks, drive) = (ran.marks, &ev.drive);
    let text = |p: &Path| fs::read_to_string(p).unwrap_or_default();
    let journal = |sid: &str| journal_of(&text(&drive.join(format!("{sid}.journal.jsonl"))));
    let decisions = |sid: &str| decisions_of(&text(&drive.join(format!("{sid}.jsonl"))));
    let acts = |sid: &str| {
        let t = tape_acts(drive, sid);
        if let Some(kept) = &ev.keep {
            let _ = fs::write(kept.join(format!("acts-{sid}.txt")), &t);
        }
        acts_of(&t)
    };
    let ledger = ledger_of(&ev.ledger);
    let messages = messages_of(&ev.messages);
    let looks = looks_of(&ev.aterm_log);
    let blocked = marks
        .blocked
        .clone()
        .unwrap_or_else(|| "the run stopped".into());
    let mut rows: Vec<(&str, Judged)> = Vec::new();
    rows.push((
        "S1",
        if marks.sid_a.is_empty() {
            Judged::not_reached(&blocked)
        } else {
            judge_s1(
                &journal(&marks.sid_a),
                &acts(&marks.sid_a),
                &ledger,
                LABEL_A,
            )
        },
    ));
    let (jb, db, ab) = if marks.sid_b.is_empty() {
        (Vec::new(), Vec::new(), Vec::new())
    } else {
        (
            journal(&marks.sid_b),
            decisions(&marks.sid_b),
            acts(&marks.sid_b),
        )
    };
    rows.push((
        "S2",
        match marks.s3 {
            Some(to) => judge_s2(&jb, &ab, &messages, to),
            None => Judged::not_reached(&blocked),
        },
    ));
    rows.push((
        "S3",
        match marks.s3 {
            Some(from) => judge_s3(
                (&jb, &db, &ab),
                (&ledger, &messages, &looks),
                LABEL_B,
                (from, marks.s3b.unwrap_or(i64::MAX)),
            ),
            None => Judged::not_reached(&blocked),
        },
    ));
    rows.push((
        "S3b",
        match (marks.s3b, marks.s3b_end) {
            (Some(from), Some(to)) => judge_s3b(
                (&jb, &db, &ab),
                &messages,
                (from, to),
                (marks.watched, ran.grace),
            ),
            _ => Judged::not_reached(&blocked),
        },
    ));
    let seen = ran.seen;
    rows.push((
        "procs",
        judge_procs(&seen.sightings, seen.control, seen.looks),
    ));
    let witness = &ran.witness;
    let opened = Judged {
        verdict: if witness.ok() {
            Verdict::Ok
        } else if witness.opened.trim().is_empty() {
            Verdict::Unproven
        } else {
            Verdict::Fail
        },
        evidence: if witness.opened.trim().is_empty() {
            format!("open.log empty; {}", crate::recorder::NOTHING_OPENED)
        } else {
            format!("open.log: {}", clip(witness.opened.trim(), 90))
        },
    };
    rows.push(("open", opened));
    rows.push(("keychain", judge_keychain(&ev.security)));
    let mut egress = judge_egress(&ev.sockets, &ev.egress);
    if !ran.witness_live && egress.verdict == Verdict::Ok {
        egress.verdict = Verdict::Unproven;
        egress
            .evidence
            .push_str("; the witness was not running at the end");
    }
    rows.push(("egress", egress));
    rows.push((
        "owner",
        judge_owner(&ev.jail_sb, &crate::paths::paths().guarded_homes()),
    ));
    rows
}

/// Print the verdict table (and keep it in `kept`, where there is one): 0 where no row is a
/// FAIL, else 1.
fn print_verdicts(run: &Run, marks: &Marks, rows: &[(&str, Judged)], kept: Option<&Path>) -> i32 {
    println!();
    println!(
        "claude-login-proof: Claude Code {} · tabs A {} B {} · accounts add: {}",
        run.claude_ver,
        marks.sid_a,
        marks.sid_b,
        clip(&marks.added, 100)
    );
    println!("  {:<8} {:<8} evidence", "id", "verdict");
    let (mut failed, mut unproven) = (0, 0);
    for (id, j) in rows {
        let word = match j.verdict {
            Verdict::Ok => "PASS",
            Verdict::Fail => {
                failed += 1;
                "FAIL"
            }
            Verdict::Unproven => {
                unproven += 1;
                "UNPROVEN"
            }
        };
        println!("  {id:<8} {word:<8} {}", j.evidence);
    }
    if let Some(kept) = kept {
        let _ = fs::write(
            kept.join("verdicts.txt"),
            rows.iter()
                .map(|(id, j)| format!("{id}\t{:?}\t{}\n", j.verdict, j.evidence))
                .collect::<String>(),
        );
    }
    if failed == 0 {
        println!(
            "claude-login-proof: PASS ({} checks{})",
            rows.len(),
            if unproven > 0 {
                format!(", {unproven} of them unproven")
            } else {
                String::new()
            }
        );
        0
    } else {
        println!(
            "claude-login-proof: FAIL ({failed} of {} checks)",
            rows.len()
        );
        1
    }
}

/// `claude-login-proof [tag] [--keep]` (and the negative control's `--roster-by-hand`, which
/// writes S3's row into the roster by hand, so no wake tells the host of it, SPECS §20). Under
/// `--keep` the lab is left up and the table judged off its live scratch, the `left` row apart.
pub fn cmd_claude_login_proof(args: &[String]) -> Res {
    let (mut tag, mut keep, mut by_hand) = ("login-proof".to_string(), false, false);
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            "--roster-by-hand" => by_hand = true,
            _ if a.starts_with('-') => {
                return Err(Fail::usage(format!("claude-login-proof: unknown {a}")));
            }
            _ => tag.clone_from(a),
        }
    }
    if !crate::paths::valid_tag(&tag) {
        return Err(Fail::die(format!("bad tag '{tag}'")));
    }
    let claude = crate::lab::find_claude().map_err(|e| {
        Fail::die(format!(
            "claude-login-proof: no Claude Code to run ({e}): install it (aterm pkg install claude) or name one with LAB_CLAUDE=/path/to/claude"
        ))
    })?;
    stop::install();
    let from_env = UpOpts::from_env();
    let instance_env = format!(
        "{} {}=1",
        from_env.instance_env,
        aterm_agent::harness::accounts_sync::UPKEEP_SEAM
    );
    let o = UpOpts {
        trust: true,
        lifeline: !keep,
        instance_env: instance_env.trim().to_string(),
        window: false,
        reroute: false,
        clock: false,
        restore: false,
        ..from_env
    };
    let run = crate::lab::up(&tag, &o).map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            Fail::die(format!(
                "claude-login-proof: up failed: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    println!(
        "claude-login-proof: Claude Code {} ({claude}) on its default route in lab '{tag}'",
        run.claude_ver
    );
    let world = World::of(&run);
    let (cfg, _) = SupervisorConfig::from_path(world.as_ref().ok().map(|w| w.config.as_path()));
    let grace = s3b_grace(&cfg);
    let mut marks = Marks::default();
    let mut watcher = None;
    let ran = world.and_then(|world| {
        lay_world(&run, &world)?;
        watcher = Some(Watcher::start(&run));
        let _ = crate::procs::jailed(
            &run,
            &run.w,
            &[],
            &[
                "/bin/sh",
                "-c",
                "/bin/sleep 2; :",
                PROC_CONTROL,
                "auth",
                "login",
            ],
            crate::procs::JailOut::Capture,
        );
        let control = crate::recorder::control(&run);
        let r = scenarios(&run, &world, by_hand, grace, &mut marks);
        Ok((world, control, r))
    });
    // What the scratch holds is read before `down` removes it.
    let store_files = ["ledger.jsonl", "logins.json", "state.json"].map(|f| {
        let p = Store::at(&run.state().join("harness").join("claude-harness"))
            .dir
            .join(f);
        (f, scratch_text(&p))
    });
    let roster = ran
        .as_ref()
        .ok()
        .map(|(world, _, _)| scratch_text(&world.roster))
        .unwrap_or_default();
    let witness_live = crate::procs::need_witness(&run).is_ok();
    let witness = ran
        .as_ref()
        .ok()
        .map(|(_, control, _)| crate::recorder::Witness::read(&run, control));
    let seen = watcher.map(Watcher::finish).unwrap_or_default();
    let left_up = || {
        println!(
            "claude-login-proof: lab '{tag}' left up ({} down {tag})",
            crate::LAB
        );
    };
    let down = if keep {
        None
    } else {
        let asked = crate::procs::lab_pids_ever(&run);
        let kept = crate::lab::down(&tag, false)?;
        let left = crate::selftest::jail_left(&run.mark, &asked);
        Some((kept, asked, left))
    };
    let outcome = match ran {
        Ok((_, _, outcome)) => outcome,
        Err(f) => {
            if keep {
                left_up();
            }
            return Err(f);
        }
    };
    if let Some((kept, _, _)) = &down {
        for (f, text) in &store_files {
            let _ = fs::write(kept.join(format!("accounts-{f}")), text);
        }
        let _ = fs::write(kept.join("roster.toml"), &roster);
        println!("lab '{tag}' is down; logs kept in {}", kept.display());
    }
    if let Err(f) = &outcome {
        if f.stopped.is_some() {
            if keep {
                left_up();
            }
            return outcome.map(|()| 0);
        }
        marks.blocked = Some(f.msg.clone().unwrap_or_default());
    }
    let ledger = store_files[0].1.clone();
    let ev = match &down {
        Some((kept, _, _)) => Evidence::kept(kept, ledger),
        None => Evidence::live(&run, ledger),
    };
    let mut rows = judge_run(
        &ev,
        &Ran {
            marks: &marks,
            seen: &seen,
            grace,
            witness: witness.unwrap_or_default(),
            witness_live,
        },
    );
    if let Some((_, asked, left)) = &down {
        rows.push((
            "left",
            Judged {
                verdict: if left.is_empty() { Verdict::Ok } else { Verdict::Fail },
                evidence: if left.is_empty() {
                    format!(
                        "nothing of the lab alive after down ({} pids asked of the kernel, the jail's mark too)",
                        asked.len()
                    )
                } else {
                    format!("alive after down: {left}")
                },
            },
        ));
    }
    let code = print_verdicts(
        &run,
        &marks,
        &rows,
        down.as_ref().map(|(k, _, _)| k.as_path()),
    );
    if keep {
        left_up();
    }
    Ok(code)
}

#[cfg(test)]
#[path = "claude_login_proof_tests.rs"]
mod tests;
