// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `claude-rotation-proof`: the real Claude Code moved between roster accounts by aterm itself,
//! end to end, on a private headless lab instance built from this tree (SPECS §21).
//!
//! The lab's Claude Code talks to the lab's stub, which the instance reads as Claude Code's
//! default route only through its development seam
//! ([`aterm_agent::harness::netwatch::LAB_ROUTE_SEAM`]): with it, a Claude Code on a stored
//! login is a roster row's process, as on the owner's machine. The world is the owner's shape:
//! a fleet file beside the roster lists three rows of this machine, and the instance's own sync
//! writes the roster from it, `lab-a` the machine's default login (the scratch HOME's `.claude`,
//! its profile naming `lab-a`'s address) and `lab-b` and `lab-c` each a login directory of the
//! accounts store. Each live fake login's token the stub reads by its label (`oat:lab-a`, never
//! its value); `lab-c` holds no login until R2. No `apiKeyHelper`, no mailbox; each credential
//! carries its own usage headers, so the usage tap reads each row's own figures. One tab runs
//! under cruise; the scenarios chain on it:
//!
//! - **R1**: `lab-a` meets its five-hour limit; aterm moves the tab onto `lab-b` with the same
//!   conversation, and `lab-b` serves the next turn with the history.
//! - **R3**: `lab-b` meets its limit while `lab-a`'s stands: no row is usable, so the tab waits
//!   for the instant `lab-a` opens again, nobody asked, and moves back onto it then.
//! - **R4** (the owner's first incident): `lab-a` at its limit again and `lab-b`'s login gone: no
//!   move, an automatic sign-in of `lab-b` tried and its outcome recorded, the tab waits on its
//!   own reset and resumes there (Claude Code's own continuation, or the host's resume once that
//!   is late), nobody asked.
//! - **R5a**, **R5b** (displacement, the owner's second incident): a sign-in on the default login
//!   as `/login` writes it (the lab writes the login and the profile, nothing is pressed) makes
//!   it `lab-b`'s, then `lab-a`'s again: each time the row it held is displaced, the row it names
//!   now is usable (R5b's was on record as signed out with a failed sign-in, as `m2-2` was), no
//!   sign-in of it is tried, and the tab's next turn runs on it.
//! - **R2**: `lab-a` at 95 % of its five-hour window (its own headers, read by the tap), `lab-c`'s
//!   login made: aterm moves the tab onto `lab-c` at a turn end, before any wall, and `lab-c`
//!   serves the next prompt with the history.
//!
//! Every key the lab sends is fenced on the composer's own row and pressed once; nothing is
//! pressed in a `/login` picker. A process watcher records every Claude Code process of the lab
//! and the login its exec environment names; the open recorder, the egress witness, the
//! keychain tripwire (each item asked for judged by the login of the launch that asked) and the
//! jail's rules are judged as `claude-login-proof` judges them. Judges are pure functions of the
//! evidence's text ([`judge_r1`] and the rest).

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aterm_agent::harness::accounts::{
    ACCOUNTS_FILE, LOGIN_ENV, RESET_GRACE_S, Recorded, Roster, StayWhy, Store,
};
use aterm_agent::harness::accounts_sync::{FLEET_ROSTER_FILE, FleetRoster, OptIn};
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::approvals::ACCOUNT_MOVE_COMMAND;
use aterm_agent::supervise::policy::guard::ROW_END;
use aterm_agent::supervise::policy::row_guard;
use aterm_agent::supervise::policy::turn_end::{
    RULE_ACCOUNT, RULE_CONTINUE, RULE_DONE_CHECK, RULE_HOST_TURN, RULE_LIMIT_RESUME, RULE_LOGIN,
    RULE_OPEN_ITEMS, RULE_SUGGESTION, RULE_UNCOMMITTED, TurnEndTiming,
};

use crate::claude_login_proof::{
    self as lp, Act, Judged, Said, Watcher, acts_of, clip, inputs, is_login_enter, is_login_send,
    journal_of, judge_egress, judge_owner, judge_procs, log_times, names, on, read_journal,
};
use crate::codex_proof::Verdict;
use crate::door::Door;
use crate::lab::UpOpts;
use crate::pyjson::J;
use crate::run::Run;
use crate::util::{atomic_write, now_ms, now_secs, scratch_text, shell_quote};
use crate::{Fail, Res, stop};

/// The first roster row: the machine's default login, the tab's at its launch.
pub const LABEL_A: &str = "lab-a";
/// The second roster row, a login directory of its own.
pub const LABEL_B: &str = "lab-b";
/// The third, whose login is made only for R2.
pub const LABEL_C: &str = "lab-c";
/// [`LABEL_A`]'s email.
pub const EMAIL_A: &str = "nobody-a@example.test";
/// [`LABEL_B`]'s email.
pub const EMAIL_B: &str = "nobody-b@example.test";
/// [`LABEL_C`]'s email.
pub const EMAIL_C: &str = "nobody-c@example.test";
/// The machine the fleet file lists the rows for (a sync finds it by the default login's
/// address, so no `[fleet] machine` is written).
pub const MACHINE: &str = "m9";
/// The stub's label of [`LABEL_A`]'s credential ([`crate::stub::auth_label`]).
pub const AUTH_A: &str = "oat:lab-a";
/// The stub's label of [`LABEL_B`]'s.
pub const AUTH_B: &str = "oat:lab-b";
/// The stub's label of [`LABEL_C`]'s.
pub const AUTH_C: &str = "oat:lab-c";
/// What [`Ev::dirs`] names the default login by: a launch with no [`LOGIN_ENV`] ([`Launched`]).
pub const DEFAULT_LOGIN: &str = "-";
/// The prompts the lab types, one a scenario step (P1 the first turn, on `lab-a`).
pub const P1: &str = "Write one sentence about lighthouses, then stop.";
/// R1's prompt, which meets `lab-a`'s limit.
pub const P2: &str = "Write one sentence about harbours, then stop.";
/// R3's, which meets `lab-b`'s.
pub const P3: &str = "Write one sentence about tides, then stop.";
/// R4's, which meets `lab-a`'s limit again.
pub const P4: &str = "Write one sentence about gulls, then stop.";
/// R5a's, after the default login was made `lab-b`'s.
pub const P5: &str = "Write one sentence about anchors, then stop.";
/// R5b's, after it was made `lab-a`'s again.
pub const P6: &str = "Write one sentence about sails, then stop.";
/// R2's, answered on `lab-a` at 95 % of its window.
pub const P7: &str = "Write one sentence about buoys, then stop.";
/// R2's next, typed once the tab moved onto `lab-c` at P7's turn end.
pub const P8: &str = "Write one sentence about jetties, then stop.";
/// How far ahead `lab-a`'s R1 limit resets: past R1's move and R3's wall, so R3 waits on it.
const R1_RESET: Duration = Duration::from_secs(3 * 60);
/// How far `lab-b`'s R3 limit resets past the instant `lab-a` opens again (its reset and the
/// pool's grace, [`RESET_GRACE_S`]): the host's wait for `lab-a` ends before `lab-b`'s own reset
/// however its notice's minute rounds it, so the move back is the one path.
const R3_PAST_A: Duration = Duration::from_secs(150);
/// How often the instance's upkeep looks (aterm-gui's `WAKE_FALLBACK`), ms: a sync or a sign-in
/// it books comes within it.
const UPKEEP_EVERY_MS: i64 = 60_000;
/// How far ahead `lab-a`'s R4 limit resets.
const R4_RESET: Duration = Duration::from_secs(3 * 60);
/// How long past R4's reset its resumed turn may come: Claude Code continues by itself at the
/// reset it named, and the host types its own resume only once that continuation is this late
/// (`TurnEndTiming::auto_resume_grace`).
fn r4_look() -> Duration {
    TurnEndTiming::default().auto_resume_grace
}
/// A wait for something that must happen: a minute in which nothing it watches moved.
const HANG: Duration = lp::HANG;
/// How long a scheduled host act may take past its instant: the host's upkeep looks once a
/// minute (aterm-gui's `WAKE_FALLBACK`), and a minute more is the hang detector.
const UPKEEP_LOOK: Duration = Duration::from_secs(120);
/// The five-hour window's figure each row's answers carry, as a fraction, before R2.
const BASELINE_5H: &str = "0.10";
/// The week's.
const BASELINE_7D: &str = "0.20";
/// `lab-a`'s five-hour figure in R2: at or above the pool's low mark (90 %).
const HIGH_5H: &str = "0.95";
/// [`LABEL_A`]'s account id, as the default login's profile names it.
const UUID_A: &str = "00000000-0000-4000-8000-00000000aa01";
/// [`LABEL_B`]'s.
const UUID_B: &str = "00000000-0000-4000-8000-00000000bb02";

// ---------------------------------------------------------------------------------------------
// The evidence, read from text.

/// One row of the accounts' ledger (`accounts/ledger.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `t`, Unix seconds.
    pub t: i64,
    /// `account`.
    pub account: String,
    /// `kind`: `wall`, `rotate`, `proven`, `sample`, `sign-in`, `displaced`, …
    pub kind: String,
    /// `to`, where a row names one.
    pub to: String,
    /// `why`.
    pub why: String,
}

/// The accounts' ledger's rows ([`Row`]).
#[must_use]
pub fn rows_of(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|l| {
            let r = J::parse(l).ok()?;
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_string();
            Some(Row {
                t: r.get("t").and_then(J::as_i64).unwrap_or(0),
                account: s("account"),
                kind: s("kind"),
                to: s("to"),
                why: s("why"),
            })
        })
        .collect()
}

/// One `/v1/messages` answer the stub logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// When, ms since the epoch ([`served_of`]).
    pub t_ms: i64,
    /// The answer's status.
    pub status: u16,
    /// `kind=`: `main` for a turn, a side call's kind otherwise.
    pub kind: String,
    /// `auth=`: the credential's label.
    pub auth: String,
    /// `mode=`: the mode it was answered in.
    pub mode: String,
    /// `last_user=`: the last user text, as the log clips it.
    pub last_user: String,
}

/// The stub log's `/v1/messages` answers ([`Served`]), their `HH:MM:SS.mmm` local stamps read
/// as ms since the epoch from `day_ms`, the local midnight of the run's first day (a stamp
/// more than 12 h before the one before it is the next day's).
#[must_use]
pub fn served_of(stub_log: &str, day_ms: i64) -> Vec<Served> {
    let mut out = Vec::new();
    let (mut day, mut last) = (day_ms, i64::MIN);
    for l in stub_log.lines() {
        let Some((stamp, rest)) = l.split_once(' ') else {
            continue;
        };
        if !rest.starts_with("POST /v1/messages -> ") {
            continue;
        }
        let Some(of_day) = stamp_ms(stamp) else {
            continue;
        };
        let mut t = day + of_day;
        if last != i64::MIN && t < last - 12 * 3_600_000 {
            day += 24 * 3_600_000;
            t += 24 * 3_600_000;
        }
        last = t;
        let word = |k: &str| {
            rest.split_whitespace()
                .find_map(|w| w.strip_prefix(k))
                .unwrap_or("")
                .to_string()
        };
        let last_user = rest
            .split_once("last_user=\"")
            .and_then(|(_, tail)| tail.rsplit_once("\" auth="))
            .map_or("", |(user, _)| user)
            .to_string();
        out.push(Served {
            t_ms: t,
            status: rest
                .strip_prefix("POST /v1/messages -> ")
                .and_then(|r| r.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            kind: word("kind="),
            auth: word("auth="),
            mode: word("mode="),
            last_user,
        });
    }
    out
}

/// `HH:MM:SS.mmm` as ms into its day.
fn stamp_ms(stamp: &str) -> Option<i64> {
    let (hms, ms) = stamp.split_once('.')?;
    let mut parts = hms.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    let ms: i64 = ms.parse().ok()?;
    (parts.next().is_none() && h < 24 && m < 60 && s < 61 && ms < 1000)
        .then_some(((h * 60 + m) * 60 + s) * 1000 + ms)
}

/// One Claude Code process of the lab, as the watcher first saw it ([`ClaudeWatch`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    /// When it was first seen, ms since the epoch.
    pub t_ms: i64,
    /// Its pid.
    pub pid: i32,
    /// The login directory its exec environment names ([`LOGIN_ENV`]), [`DEFAULT_LOGIN`] where
    /// none (or an empty one): the machine's default login.
    pub login: String,
    /// Its argv carries `--resume`.
    pub resumed: bool,
}

/// A launch as the evidence keeps it: `<t_ms> <pid> <login> <resumed>`.
#[must_use]
pub fn launch_line(l: &Launched) -> String {
    format!(
        "{} {} {} {}",
        l.t_ms,
        l.pid,
        l.login,
        if l.resumed { "resume" } else { "fresh" }
    )
}

/// The launches [`launch_line`] wrote.
#[must_use]
pub fn launches_of(text: &str) -> Vec<Launched> {
    text.lines()
        .filter_map(|l| {
            let (head, resumed) = l.rsplit_once(' ')?;
            let mut w = head.splitn(3, ' ');
            Some(Launched {
                t_ms: w.next()?.parse().ok()?,
                pid: w.next()?.parse().ok()?,
                login: w.next()?.to_string(),
                resumed: match resumed {
                    "resume" => true,
                    "fresh" => false,
                    _ => return None,
                },
            })
        })
        .collect()
}

/// What a scenario's run left for its judge: its window, and the history of the first turn the
/// row it moved to served ([`Marks`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leg {
    /// When the lab set the scenario's condition (a limit, a lost login, a sign-in on the
    /// default login, a high figure), ms.
    pub from: i64,
    /// When the next scenario began (or the run ended), ms.
    pub to: i64,
    /// The instant the condition's limit resets, Unix seconds (0: none).
    pub reset: i64,
    /// When the lab typed the scenario's prompt, ms (0: none typed).
    pub asked: i64,
    /// The user texts of the first turn served after the scenario's move, resume or prompt, as
    /// the stub's dump holds them.
    pub history: Vec<String>,
    /// `aterm harness accounts --json` as the instance's own CLI printed it just before the
    /// scenario's condition was set (R5 alone).
    pub before: String,
    /// The same once the scenario's turn was served (R5 alone).
    pub listing: String,
    /// Why the run stopped in this scenario, where it did.
    pub blocked: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// The judges.

/// The evidence every scenario's judge reads.
#[derive(Debug, Clone, Default)]
pub struct Ev {
    /// The tab's journal.
    pub journal: Vec<Said>,
    /// The accounts' ledger.
    pub ledger: Vec<Row>,
    /// The instance's aterm.log.
    pub aterm_log: String,
    /// The stub's `/v1/messages` answers.
    pub served: Vec<Served>,
    /// The lab's Claude Code processes.
    pub launches: Vec<Launched>,
    /// The harness's upgrade ledger (`upgrade/ledger.jsonl`), the restarts' steps.
    pub drive_ledger: String,
    /// Each row's login as a launch names it ([`Launched::login`]), by label.
    pub dirs: Vec<(String, String)>,
    /// The tab's sid, as the upgrade ledger's rows name it (`tab`).
    pub sid: String,
}

impl Ev {
    fn dir(&self, label: &str) -> &str {
        self.dirs
            .iter()
            .find(|(l, _)| l == label)
            .map_or("", |(_, d)| d.as_str())
    }
}

/// Whether a journal record is a harness act typed into the tab (`TYPED` for a command,
/// `CONTINUED` for a prompt).
#[must_use]
pub fn is_typed(line: &str) -> bool {
    line.starts_with("TYPED ") || line.starts_with("CONTINUED ")
}

/// Whether a journal record is a harness act typed for `rule` ([`is_typed`]).
#[must_use]
pub fn typed_for(line: &str, rule: &str) -> bool {
    is_typed(line)
        && line
            .split_whitespace()
            .any(|w| w.strip_prefix("rule=") == Some(rule))
}

/// `RESTARTED … rule=account-rotate@v1 relaunch under account <to>` records within the window,
/// the first.
fn restarted<'a>(journal: &'a [Said], to: &str, (from, until): (i64, i64)) -> Option<&'a Said> {
    let what = format!(" rule={RULE_ACCOUNT} {ACCOUNT_MOVE_COMMAND}{to}");
    journal.iter().find(|s| {
        (from..=until).contains(&s.t_ms)
            && s.line.starts_with("RESTARTED ")
            && s.line.contains(&what)
            && names(&s.line, to)
    })
}

/// Any `RESTARTED … rule=account-rotate@v1` record within the window, the first.
fn any_restart(journal: &[Said], (from, until): (i64, i64)) -> Option<&Said> {
    let what = format!(" rule={RULE_ACCOUNT} ");
    journal.iter().find(|s| {
        (from..=until).contains(&s.t_ms)
            && s.line.starts_with("RESTARTED ")
            && s.line.contains(&what)
    })
}

/// When the ledger books `kind` for `label` at or after `from_s` with a row that `why` admits.
fn booked_after(
    ledger: &[Row],
    label: &str,
    kind: &str,
    from_s: i64,
    why: impl Fn(&Row) -> bool,
) -> Option<i64> {
    ledger
        .iter()
        .find(|r| r.account == label && r.kind == kind && r.t >= from_s && why(r))
        .map(|r| r.t)
}

/// The first `main` turn the stub answered 200 at or after `at`, which must carry `auth`.
fn first_turn_on<'a>(served: &'a [Served], auth: &str, at: i64) -> Result<&'a Served, String> {
    match served
        .iter()
        .find(|s| s.t_ms >= at && s.kind == "main" && s.status == 200)
    {
        Some(s) if s.auth == auth => Ok(s),
        Some(s) => Err(format!(
            "the first turn served after it carried {} ({}), not {auth}",
            s.auth, s.status
        )),
        None => Err(format!("no turn served after it under {auth}")),
    }
}

/// The prompts of `prompts` that `history` (a turn's user texts) does not hold.
fn holds_history(history: &[String], prompts: &[&str]) -> Vec<String> {
    prompts
        .iter()
        .filter(|p| !history.iter().any(|h| h.contains(**p)))
        .map(|p| format!("the history lacks `{}`", clip(p, 40)))
        .collect()
}

/// What is wrong with the process the window's account restart adopted ([`adopted_pids`]): none
/// adopted, never seen launched, or launched on another login than `to`'s ([`Launched::login`]).
fn adopted_wrong(ev: &Ev, to: &str, w: (i64, i64)) -> Option<String> {
    let adopted = adopted_pids(&ev.aterm_log, w);
    let Some(&first) = adopted.first() else {
        return Some("aterm.log names no process a restart:account adopted".into());
    };
    match adopted
        .iter()
        .find_map(|pid| ev.launches.iter().find(|l| l.pid == *pid))
    {
        None => Some(format!(
            "the adopted process (pid {first}) was never seen launched"
        )),
        Some(l) if l.login != ev.dir(to) => Some(format!(
            "the adopted process (pid {}) runs on login `{}`, not {to}'s",
            l.pid, l.login
        )),
        Some(_) => None,
    }
}

/// The process each account restart in the window adopted (aterm.log's `harness @<sid>:
/// restart:account pid=<pid> … step=adopted`, or `restart:account-ahead` for a move ahead of a
/// wall). The restart starts its process before it adopts it, so the launch is dated before the
/// journal's `RESTARTED` record; its pid ties the two.
#[must_use]
pub fn adopted_pids(aterm_log: &str, (from, until): (i64, i64)) -> Vec<i32> {
    aterm_log
        .lines()
        .filter(|l| l.contains(" step=adopted"))
        .filter(|l| {
            log_times(l, "restart:account")
                .first()
                .is_some_and(|t| (from..=until).contains(t))
        })
        .filter_map(|l| {
            let rest = [": restart:account pid=", ": restart:account-ahead pid="]
                .iter()
                .find_map(|step| l.split_once(step).map(|(_, rest)| rest))?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .collect()
}

/// One row of the harness's upgrade ledger (`upgrade/ledger.jsonl`): one step of a restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriveStep {
    /// `t`, Unix seconds.
    pub t: i64,
    /// `step`: `restart:account` (a move at a wall), `restart:account-ahead` (one ahead of it),
    /// `relaunched`, `adopted`, `done:resumed`, …
    pub step: String,
    /// `tab`: the session's sid.
    pub tab: String,
    /// `pid`: the process the step is about (the one a restart step ends, the one `adopted`
    /// took).
    pub pid: i64,
}

/// The upgrade ledger's rows ([`DriveStep`]); a line that does not parse reads as nothing.
#[must_use]
pub fn drive_steps_of(text: &str) -> Vec<DriveStep> {
    text.lines()
        .filter_map(|l| {
            let r = J::parse(l).ok()?;
            let s = |k: &str| r.get(k).and_then(J::as_str).unwrap_or("").to_string();
            Some(DriveStep {
                t: r.get("t").and_then(J::as_i64).unwrap_or(0),
                step: s("step"),
                tab: s("tab"),
                pid: r.get("pid").and_then(J::as_i64).unwrap_or(0),
            })
        })
        .collect()
}

/// What is wrong with the upgrade ledger's record of the window's restart: no row of the tab
/// whose step is exactly `step` stamped within the window (its whole second), or the restart it
/// opens adopted no process aterm.log's adopted lines of the window name ([`adopted_pids`]): the
/// first row of the tab after it that is `adopted` or another restart step must be an `adopted`
/// of one of those pids.
fn stepped_wrong(ev: &Ev, step: &str, (from, until): (i64, i64)) -> Option<String> {
    let rows = drive_steps_of(&ev.drive_ledger);
    let at = rows.iter().position(|r| {
        r.step == step && r.tab == ev.sid && r.t * 1000 + 999 >= from && r.t * 1000 <= until
    });
    let Some(at) = at else {
        return Some(format!(
            "the upgrade ledger has no {step} step of the tab in the window"
        ));
    };
    let adopted = adopted_pids(&ev.aterm_log, (from, until));
    let next = rows[at + 1..]
        .iter()
        .find(|r| r.tab == ev.sid && (r.step == "adopted" || r.step.starts_with("restart:")));
    match next {
        Some(r) if r.step == "adopted" && adopted.iter().any(|p| i64::from(*p) == r.pid) => None,
        Some(r) if r.step == "adopted" => Some(format!(
            "the upgrade ledger's {step} step adopted pid {}, which aterm.log's adopted lines of the window do not name",
            r.pid
        )),
        _ => Some(format!(
            "the upgrade ledger's {step} step is followed by no `adopted` row of the tab"
        )),
    }
}

/// The aterm.log lines within the window that hold `needle`.
fn logged(aterm_log: &str, needle: &str, (from, until): (i64, i64)) -> Vec<i64> {
    log_times(aterm_log, needle)
        .into_iter()
        .filter(|t| (from..=until).contains(t))
        .collect()
}

/// R1: `lab-a`'s wall at `leg.from`. The host logged its advice to move to `lab-b`;
/// the ledger booked `lab-a`'s five-hour wall, the rotation onto `lab-b` and `lab-b` proven; the
/// journal holds the account restart onto `lab-b` from the limit and then a [`RULE_LIMIT_RESUME`]
/// continuation; the upgrade ledger the tab's own `restart:account` step within the window, and
/// the process it adopted is the one aterm.log names ([`stepped_wrong`]); a process on `lab-b`'s
/// login was started; the first turn served after the restart carried `lab-b`'s credential, and
/// its history holds P1 and P2.
#[must_use]
pub fn judge_r1(ev: &Ev, leg: &Leg) -> Judged {
    judge_wall_move(ev, leg, (LABEL_A, LABEL_B, AUTH_B), &[P1, P2])
}

/// The wall move R1 and R3's second half judge ([`judge_r1`]): `from` walled, the tab moved to
/// `to`, which served the next turn under `auth` with `prompts` in its history.
fn judge_wall_move(
    ev: &Ev,
    leg: &Leg,
    (from, to, auth): (&str, &str, &str),
    prompts: &[&str],
) -> Judged {
    let mut missing = Vec::new();
    let w = (leg.from, leg.to);
    let from_s = leg.from / 1000 - 1;
    if logged(&ev.aterm_log, &format!(": account advice: move to {to}"), w).is_empty() {
        missing.push(format!("aterm.log has no `account advice: move to {to}`"));
    }
    if booked_after(&ev.ledger, from, "wall", from_s, |r| {
        r.why.contains("five_hour")
    })
    .is_none()
    {
        missing.push(format!("no five_hour `wall` row for {from}"));
    }
    let rotated = booked_after(&ev.ledger, from, "rotate", from_s, |r| r.to == to);
    if rotated.is_none() {
        missing.push(format!("no `rotate` row {from} → {to}"));
    }
    if booked_after(
        &ev.ledger,
        to,
        "proven",
        rotated.unwrap_or(i64::MAX),
        |_| true,
    )
    .is_none()
    {
        missing.push(format!("no `proven` row for {to} after the rotation"));
    }
    let moved = restarted(&ev.journal, to, w);
    match moved {
        None => missing.push(format!("no RESTARTED {RULE_ACCOUNT} onto {to}")),
        Some(s)
            if !s
                .line
                .contains("(from the limit its turn ended on; the same conversation)") =>
        {
            missing.push(format!(
                "the restart is not from the limit: `{}`",
                clip(&s.line, 90)
            ));
        }
        Some(_) => {}
    }
    let at = moved.map_or(leg.to, |s| s.t_ms);
    if !ev
        .journal
        .iter()
        .any(|s| (at..=leg.to).contains(&s.t_ms) && typed_for(&s.line, RULE_LIMIT_RESUME))
    {
        missing.push(format!("no {RULE_LIMIT_RESUME} act after the restart"));
    }
    missing.extend(stepped_wrong(ev, "restart:account", w));
    missing.extend(adopted_wrong(ev, to, w));
    if let Err(why) = first_turn_on(&ev.served, auth, at) {
        missing.push(why);
    }
    missing.extend(holds_history(&leg.history, prompts));
    Judged::of(
        &missing,
        format!(
            "{from} walled; `{}`; {to} served the next turn ({auth}) with {} in its history",
            clip(moved.map_or("", |s| s.line.as_str()), 80),
            prompts
                .iter()
                .map(|p| clip(p, 18))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// The instant a `WAITING … until=<ISO>Z …` record names, ms since the epoch.
#[must_use]
pub fn waiting_until(line: &str) -> Option<i64> {
    let iso = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("until="))?;
    crate::workers::iso_ms(iso)
}

/// The `until` instant of an `account advice: wait on <label> (until <unix s>)` aterm.log line
/// at or after `from` (ms), the first.
#[must_use]
pub fn advice_until(aterm_log: &str, label: &str, from: i64) -> Option<i64> {
    let needle = format!(": account advice: wait on {label} (until ");
    aterm_log.lines().find_map(|l| {
        let secs: f64 = l.split_whitespace().next()?.parse().ok()?;
        if ((secs * 1000.0).round() as i64) < from {
            return None;
        }
        let (_, rest) = l.split_once(&needle)?;
        rest.split(')').next()?.trim().parse().ok()
    })
}

/// R3: `lab-b`'s wall at `leg.from` while `lab-a`'s limit (resetting at `a_reset`, Unix s)
/// stands. The host advised a wait on `lab-b` until `lab-a` opens (`a_reset` and no later than
/// its grace), a `WAITING` record names that instant, nobody was asked, and from that instant the
/// tab moved back onto `lab-a`, which served the next turn with P1, P2 and P3 in its history.
#[must_use]
pub fn judge_r3(ev: &Ev, leg: &Leg, a_reset: i64) -> Judged {
    let mut missing = Vec::new();
    let w = (leg.from, leg.to);
    let until = advice_until(&ev.aterm_log, LABEL_B, leg.from);
    match until {
        None => missing.push(format!(
            "aterm.log has no `account advice: wait on {LABEL_B} (until …)`"
        )),
        Some(u) if !(a_reset..=a_reset + 600).contains(&u) => missing.push(format!(
            "the advice waits until {u}, not {LABEL_A}'s opening ({a_reset} and its grace)"
        )),
        Some(_) => {}
    }
    let waiting = ev.journal.iter().find(|s| {
        (leg.from..=leg.to).contains(&s.t_ms)
            && s.line.starts_with("WAITING ")
            && waiting_until(&s.line)
                .is_some_and(|t| until.is_some_and(|u| (t / 1000 - u).abs() <= 120))
    });
    if waiting.is_none() {
        missing.push("no WAITING record naming the instant the advice waits for".into());
    }
    let moved = restarted(&ev.journal, LABEL_A, w);
    if let (Some(m), Some(u)) = (moved, until)
        && m.t_ms < u * 1000 - 5_000
    {
        missing.push(format!(
            "the move back came {} s before {LABEL_A} opened",
            (u * 1000 - m.t_ms) / 1000
        ));
    }
    let wall = judge_wall_move(ev, leg, (LABEL_B, LABEL_A, AUTH_A), &[P1, P2, P3]);
    if wall.verdict != Verdict::Ok {
        missing.push(wall.evidence.clone());
    }
    Judged::of(
        &missing,
        format!(
            "no row usable: `{}`; {}",
            clip(waiting.map_or("", |s| s.line.as_str()), 70),
            wall.evidence
        ),
    )
}

/// How far past the tab's own reset its usage-session wait may name an instant: the host's resume
/// once Claude Code's own continuation is late (`TurnEndTiming::auto_resume_grace`), or, where
/// Claude Code does not continue by itself, its resume at the reset with its grace and jitter;
/// and a minute more, for the reset as Claude Code names it, to the minute.
fn r4_wait_past_reset() -> Duration {
    let t = TurnEndTiming::default();
    t.auto_resume_grace.max(t.reset_grace + t.reset_jitter) + Duration::from_secs(60)
}

/// The reason a `WAITING … until=<ISO>Z <why>` record gives, the words after its instant.
#[must_use]
pub fn waiting_why(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once(" until=")?;
    Some(rest.split_once(' ').map_or("", |(_, why)| why))
}

/// R4: `lab-a`'s wall at `leg.from` with `lab-b`'s login gone. No restart onto any row; the
/// accounts' ledger books an automatic sign-in of `lab-b` and the routes it tried (`auto …`);
/// a `WAITING` record of the usage-session wall names an instant from a minute before the tab's
/// own reset to [`r4_wait_past_reset`] past it (the host's own resume deadline, which is no
/// weekly reset or other far instant); no [`RULE_LIMIT_RESUME`] continuation is typed before
/// the reset; and the first turn served from the reset on (Claude Code's own continuation at the
/// reset it named, or the host's resume once that is late) is `lab-a`'s, with P1 and P4 in its
/// history.
#[must_use]
pub fn judge_r4(ev: &Ev, leg: &Leg) -> Judged {
    let mut missing = Vec::new();
    let w = (leg.from, leg.to);
    if let Some(s) = any_restart(&ev.journal, w) {
        missing.push(format!("moved: `{}`", clip(&s.line, 80)));
    }
    let signed = ev.ledger.iter().find(|r| {
        r.account == LABEL_B
            && r.kind == "sign-in"
            && r.t >= leg.from / 1000 - 1
            && r.why.starts_with("auto ")
    });
    if signed.is_none() {
        missing.push(format!(
            "no automatic `sign-in` row for {LABEL_B} after its login went"
        ));
    }
    let past = i64::try_from(r4_wait_past_reset().as_millis()).unwrap_or(i64::MAX);
    let names = (leg.reset * 1000 - 60_000)..=(leg.reset * 1000).saturating_add(past);
    let usage = aterm_phase::WallKind::UsageSession.name();
    let waiting = ev.journal.iter().find(|s| {
        (leg.from..=leg.to).contains(&s.t_ms)
            && s.line.starts_with("WAITING ")
            && waiting_until(&s.line).is_some_and(|t| names.contains(&t))
            && waiting_why(&s.line).is_some_and(|why| why.starts_with(usage))
    });
    if waiting.is_none() {
        missing.push(format!(
            "no WAITING record of the {usage} wall naming the tab's own reset (from a minute \
             before it to {} s past it)",
            past / 1000
        ));
    }
    let opens = leg.reset * 1000 - 5_000;
    if let Some(s) = ev
        .journal
        .iter()
        .find(|s| (leg.from..opens).contains(&s.t_ms) && typed_for(&s.line, RULE_LIMIT_RESUME))
    {
        missing.push(format!(
            "a resume typed before the reset: `{}`",
            clip(&s.line, 70)
        ));
    }
    if let Err(why) = first_turn_on(&ev.served, AUTH_A, opens) {
        missing.push(why);
    }
    let served = ev
        .served
        .iter()
        .find(|s| s.t_ms >= opens && s.kind == "main" && s.status == 200);
    let by = match served {
        None => "no turn served",
        Some(s)
            if ev.journal.iter().any(|j| {
                (opens..=s.t_ms).contains(&j.t_ms) && typed_for(&j.line, RULE_LIMIT_RESUME)
            }) =>
        {
            "the host's resume"
        }
        Some(_) => "Claude Code's own continuation",
    };
    missing.extend(holds_history(&leg.history, &[P1, P4]));
    Judged::of(
        &missing,
        format!(
            "no move; {LABEL_B}: `{}`; `{}`; {by} served on {LABEL_A} at its reset",
            clip(signed.map_or("", |r| r.why.as_str()), 60),
            clip(waiting.map_or("", |s| s.line.as_str()), 60)
        ),
    )
}

/// One row of `aterm harness accounts --json` ([`listed`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listed {
    /// `auth`: `ok`, `signed-out` or `unseen`.
    pub auth: String,
    /// `login`: `ok`, `idle`, `due` or `expired` (`accounts::LoginState`).
    pub login: String,
    /// `condition`: `usable`, `limited`, `signed-out`, `not-enrolled`, …
    pub condition: String,
    /// `sign_in_attempt.outcome`, empty where none is on record.
    pub sign_in: String,
}

/// The row `label` of an `aterm harness accounts --json` listing, where it lists one.
#[must_use]
pub fn listed(listing: &str, label: &str) -> Option<Listed> {
    let j = J::parse(listing.trim()).ok()?;
    let J::Arr(rows) = j.get("accounts")? else {
        return None;
    };
    let row = rows
        .iter()
        .find(|r| r.get("label").and_then(J::as_str) == Some(label))?;
    let s = |k: &str| row.get(k).and_then(J::as_str).unwrap_or("").to_string();
    Some(Listed {
        auth: s("auth"),
        login: s("login"),
        condition: s("condition"),
        sign_in: row
            .get("sign_in_attempt")
            .and_then(|a| a.get("outcome"))
            .and_then(J::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// R5a and R5b: from `leg.from` the default login holds `to`'s account, as a `/login` on it
/// writes it, where it held `from`'s ([`judge_r5`]).
#[derive(Debug, Clone, Copy)]
pub struct Displacement<'a> {
    /// The row the default login held.
    pub from: &'a str,
    /// The row it names now.
    pub to: &'a str,
    /// `to`'s credential, as the stub logs it.
    pub auth: &'a str,
    /// The prompts the next turn's history must hold.
    pub prompts: &'a [&'a str],
}

/// Whether a sign-in attempt's outcome (`sign_in_attempt.outcome`) left its row without a login.
fn failing(outcome: &str) -> bool {
    matches!(outcome, "no-route" | "failed")
}

/// R5a and R5b: the default login made `d.to`'s at `leg.from`. The ledger books `d.from`
/// displaced onto `d.to`; where the instance's listing before (`leg.before`) showed `d.to` signed
/// out, its `auth` row back to `ok`; no automatic sign-in of `d.to` after the displacement; no
/// account restart in the window (the default login moved, not the session); the listing after
/// (`leg.listing`) shows `d.to` with auth `ok`, its login not `expired` and no failed sign-in on
/// record; and the first turn served after the lab's prompt (`leg.asked`) carried `d.auth`, its
/// history holding `d.prompts`. Unproven where all that holds but the listing before showed
/// nothing stale against `d.to` (no sign-out, no failed sign-in), which a sync with no fix would
/// pass as well.
#[must_use]
pub fn judge_r5(ev: &Ev, leg: &Leg, d: Displacement<'_>) -> Judged {
    let mut missing = Vec::new();
    let from_s = leg.from / 1000 - 1;
    let before = listed(&leg.before, d.to);
    let was_out = before.as_ref().is_some_and(|r| r.auth == "signed-out");
    let had_failed = before.as_ref().is_some_and(|r| failing(&r.sign_in));
    let displaced = booked_after(&ev.ledger, d.from, "displaced", from_s, |r| r.to == d.to);
    if displaced.is_none() {
        missing.push(format!("no `displaced` row {} → {}", d.from, d.to));
    }
    if was_out && booked_after(&ev.ledger, d.to, "auth", from_s, |r| r.why == "ok").is_none() {
        missing.push(format!("no `auth ok` row for {} after the sign-in", d.to));
    }
    if let Some(r) = ev
        .ledger
        .iter()
        .find(|r| r.account == d.to && r.kind == "sign-in" && r.t >= displaced.unwrap_or(from_s))
    {
        missing.push(format!(
            "a sign-in of {} tried after it was signed in: `{}`",
            d.to,
            clip(&r.why, 60)
        ));
    }
    if let Some(s) = any_restart(&ev.journal, (leg.from, leg.to)) {
        missing.push(format!("restarted: `{}`", clip(&s.line, 80)));
    }
    let after = listed(&leg.listing, d.to);
    match &after {
        None => missing.push(format!("the instance's listing has no row {}", d.to)),
        Some(row) => {
            if row.auth != "ok" {
                missing.push(format!("{} listed auth {}", d.to, row.auth));
            }
            if row.login == "expired" {
                missing.push(format!("{} listed login expired", d.to));
            }
            if failing(&row.sign_in) {
                missing.push(format!(
                    "{} listed a failed sign-in ({})",
                    d.to, row.sign_in
                ));
            }
        }
    }
    let asked = if leg.asked > 0 { leg.asked } else { leg.from };
    if let Err(why) = first_turn_on(&ev.served, d.auth, asked) {
        missing.push(why);
    }
    missing.extend(holds_history(&leg.history, d.prompts));
    let word = |r: &Option<Listed>| {
        r.as_ref().map_or_else(
            || "unlisted".to_string(),
            |r| {
                format!(
                    "auth {} login {} {}{}",
                    r.auth,
                    r.login,
                    r.condition,
                    if r.sign_in.is_empty() {
                        String::new()
                    } else {
                        format!(" sign-in {}", r.sign_in)
                    }
                )
            },
        )
    };
    let judged = Judged::of(
        &missing,
        format!(
            "{} displaced onto {}; {} was {}, now {}; its next turn on {} with the history; no sign-in tried",
            d.from,
            d.to,
            d.to,
            word(&before),
            word(&after),
            d.auth
        ),
    );
    if judged.verdict == Verdict::Ok && !(was_out || had_failed) {
        return Judged {
            verdict: Verdict::Unproven,
            evidence: format!(
                "{}; but nothing stale stood against {} before (no sign-out, no failed sign-in)",
                judged.evidence, d.to
            ),
        };
    }
    judged
}

/// The session figure a ledger row's `why` names (`tap: session N% week M%`, `session N%`).
fn session_pct(why: &str) -> Option<u32> {
    why.split_once("session ")
        .and_then(|(_, rest)| rest.split('%').next())
        .and_then(|p| p.trim().parse().ok())
}

/// R2: `lab-a` answered at a high figure from `leg.from`. The ledger booked the usage tap's
/// reading of `lab-a` at 90 % or more of its session window (a `sample` row, `tap: session N%`)
/// no later than the rotation onto `lab-c`, then `lab-c` proven; a `warned` row (Claude Code's own
/// warning, read off its screen) does not stand for it, and the evidence says whether one was
/// booked too. The journal holds the account restart onto `lab-c` at a turn end, and the upgrade
/// ledger its `restart:account-ahead` step ([`stepped_wrong`]); no turn of `lab-a`'s was walled
/// (no 429) in the window; the process that restart adopted was started on `lab-c`'s login;
/// `lab-c` served the next turn (the lab's P8, typed once the tab moved) with P1, P7 and P8 in
/// its history.
#[must_use]
pub fn judge_r2(ev: &Ev, leg: &Leg) -> Judged {
    let mut missing = Vec::new();
    let w = (leg.from, leg.to);
    let from_s = leg.from / 1000 - 1;
    let rotated = booked_after(&ev.ledger, LABEL_A, "rotate", from_s, |r| r.to == LABEL_C);
    let tapped = ev.ledger.iter().find(|r| {
        r.account == LABEL_A
            && r.kind == "sample"
            && r.why.starts_with("tap: ")
            && r.t >= from_s
            && rotated.is_none_or(|t| r.t <= t)
            && session_pct(&r.why).is_some_and(|p| p >= 90)
    });
    if tapped.is_none() {
        missing.push(format!(
            "no tap `sample` row for {LABEL_A} at 90 % or more before the rotation"
        ));
    }
    let warned = booked_after(&ev.ledger, LABEL_A, "warned", from_s, |r| {
        session_pct(&r.why).is_some_and(|p| p >= 90)
    });
    if rotated.is_none() {
        missing.push(format!("no `rotate` row {LABEL_A} → {LABEL_C}"));
    }
    if booked_after(
        &ev.ledger,
        LABEL_C,
        "proven",
        rotated.unwrap_or(i64::MAX),
        |_| true,
    )
    .is_none()
    {
        missing.push(format!("no `proven` row for {LABEL_C} after the rotation"));
    }
    let moved = restarted(&ev.journal, LABEL_C, w);
    match moved {
        None => missing.push(format!("no RESTARTED {RULE_ACCOUNT} onto {LABEL_C}")),
        Some(s) if !s.line.contains("(at a turn end; the same conversation)") => {
            missing.push(format!(
                "the restart is not at a turn end: `{}`",
                clip(&s.line, 90)
            ));
        }
        Some(_) => {}
    }
    if let Some(s) = ev
        .served
        .iter()
        .find(|s| (leg.from..=leg.to).contains(&s.t_ms) && s.auth == AUTH_A && s.status == 429)
    {
        missing.push(format!(
            "{LABEL_A} was walled in the window ({} {})",
            s.status, s.mode
        ));
    }
    let at = moved.map_or(leg.to, |s| s.t_ms);
    missing.extend(stepped_wrong(ev, "restart:account-ahead", w));
    missing.extend(adopted_wrong(ev, LABEL_C, w));
    if let Err(why) = first_turn_on(&ev.served, AUTH_C, at) {
        missing.push(why);
    }
    missing.extend(holds_history(&leg.history, &[P1, P7, P8]));
    Judged::of(
        &missing,
        format!(
            "{LABEL_A} read at {} % by the tap{}, no wall; `{}`; {LABEL_C} served the next turn with the history",
            tapped
                .and_then(|r| session_pct(&r.why))
                .map_or_else(|| "?".to_string(), |p| p.to_string()),
            if warned.is_some() {
                " (Claude Code's warning read off its screen too)"
            } else {
                ""
            },
            clip(moved.map_or("", |s| s.line.as_str()), 80)
        ),
    )
}

/// The stub: every `/v1/messages` request carried a roster row's credential (`oat:lab-a`,
/// `oat:lab-b` or `oat:lab-c`), so every session ran on a stored login; unproven unless each of
/// them carried one.
#[must_use]
pub fn judge_stub(served: &[Served]) -> Judged {
    let ours = [AUTH_A, AUTH_B, AUTH_C];
    let other: Vec<&Served> = served
        .iter()
        .filter(|s| !ours.contains(&s.auth.as_str()))
        .collect();
    let counts = ours.map(|a| served.iter().filter(|s| s.auth == a).count());
    Judged {
        verdict: if !other.is_empty() {
            Verdict::Fail
        } else if counts.contains(&0) {
            Verdict::Unproven
        } else {
            Verdict::Ok
        },
        evidence: match other.first() {
            Some(s) => format!(
                "{} request(s) on another credential, the first auth={} ({})",
                other.len(),
                s.auth,
                s.status
            ),
            None => format!(
                "{} requests: {} on {AUTH_A}, {} on {AUTH_B}, {} on {AUTH_C}, none on another credential",
                served.len(),
                counts[0],
                counts[1],
                counts[2]
            ),
        },
    }
}

/// One line of the instance's `messages.log` (aterm-messages' `LogLine`), the fields the asked
/// judge reads ([`notes_of`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    /// `kind`: `posted`, `retired`, `acted` or `dropped`.
    pub kind: String,
    /// `id`: the message the line is about.
    pub id: String,
    /// `t`, ms since the epoch.
    pub t_ms: i64,
    /// `title` (a retired line's: its final words).
    pub title: String,
    /// `detail`'s lines.
    pub detail: Vec<String>,
    /// `key`: the message's supersede key.
    pub key: String,
    /// `sev`: `success`, `info`, `warn` or `error`.
    pub sev: String,
    /// `need=later`: an entry of the exceptions digest, a fact for later that never interrupts.
    pub later: bool,
    /// A retired line's `how`: `folded`, `resolved:ok`, `dismissed`, …
    pub how: String,
    /// A retired line's `rec=1`: a record, the log only, retired as it was posted.
    pub record: bool,
    /// A retired line's `rep`: the posts folded into the message, its first one counted.
    pub rep: u32,
    /// A dropped line's `n`: lines the log lost before it.
    pub dropped: u32,
}

/// A `messages.log` field's text, its escapes (`\\`, `\t`, `\n`, `\r`, and `\u` for the unit
/// separator between detail lines) undone; an unknown escape keeps both characters.
fn unescape_field(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') | None => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('u') => out.push('\u{1f}'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// `messages.log`'s lines ([`Note`]): each `m1` line with a `kind`; other lines read as nothing.
#[must_use]
pub fn notes_of(text: &str) -> Vec<Note> {
    text.lines()
        .filter_map(|l| {
            let mut fields = l.split('\t');
            if fields.next() != Some("m1") {
                return None;
            }
            let pairs: Vec<(&str, String)> = fields
                .filter_map(|f| f.split_once('='))
                .map(|(k, v)| (k, unescape_field(v)))
                .collect();
            let field = |k: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.clone())
            };
            let detail = field("detail").unwrap_or_default();
            Some(Note {
                kind: field("kind")?,
                id: field("id").unwrap_or_default(),
                t_ms: field("t").and_then(|t| t.parse().ok()).unwrap_or(0),
                title: field("title").unwrap_or_default(),
                detail: if detail.is_empty() {
                    Vec::new()
                } else {
                    detail.split('\u{1f}').map(str::to_string).collect()
                },
                key: field("key").unwrap_or_default(),
                sev: field("sev").unwrap_or_default(),
                later: field("need").as_deref() == Some("later"),
                how: field("how").unwrap_or_default(),
                record: field("rec").as_deref() == Some("1"),
                rep: field("rep").and_then(|r| r.parse().ok()).unwrap_or(0),
                dropped: field("n").and_then(|n| n.parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

/// The accounts' facts for later (aterm-gui's `harness.accounts.<kind>` digest keys).
const ACCOUNTS_KEYS: &str = "harness.accounts.";

/// The one accounts' fact for later this proof allows: the recorded error of a row whose
/// automatic sign-in found no route after every route was tried (aterm-gui's
/// `ACCOUNTS_NO_SIGN_IN_KEY`), which asks no one to sign in.
const NO_SIGN_IN_KEY: &str = "harness.accounts.no-sign-in";

/// Whether a message's words name `/login`: an ask, or a report, of a sign-in by hand.
fn names_login(n: &Note) -> bool {
    n.title.contains("/login") || n.detail.iter().any(|d| d.contains("/login"))
}

/// Nobody asked, over the instance's whole `messages.log` (the instance is the run's own, so
/// every line from its start is this run's) and the tab's whole journal. Each message posted is
/// judged by its class and severity, whenever it was posted: one whose words (at its post or its
/// retirement) name `/login`, or of severity `error`, is a Fail; a fact for later (`need=later`,
/// which never interrupts) is allowed and named in the evidence with the posts folded into it
/// (its retired line's `rep`, or `open` where none retired it), unless it is an accounts fact
/// other than the recorded no-route error ([`NO_SIGN_IN_KEY`]), which is a Fail; a record (retired
/// `folded` with `rec=1`) is allowed and counted; any other message stood on the owner's screen
/// and is a Fail. A `dropped` line leaves the judge blind to what the log lost: Unproven where
/// nothing failed. The journal holds no `EXCEPTION`, `ESCALATED`, `NEEDS` or `LEFT` record and
/// no `TYPED` of [`RULE_LOGIN`].
#[must_use]
pub fn judge_asked(journal: &[Said], notes: &[Note]) -> Judged {
    let mut missing = Vec::new();
    let (mut later, mut records, mut lost) = (Vec::new(), 0, 0);
    for posted in notes.iter().filter(|n| n.kind == "posted") {
        let end = notes
            .iter()
            .rev()
            .find(|n| n.kind == "retired" && n.id == posted.id);
        let words = format!("`{}`", clip(&posted.title, 60));
        if names_login(posted) || end.is_some_and(names_login) {
            missing.push(format!("a message names /login: {words}"));
        } else if posted.sev == "error" || end.is_some_and(|e| e.sev == "error") {
            missing.push(format!("an error: {words}"));
        } else if posted.later {
            if posted.key.starts_with(ACCOUNTS_KEYS) && posted.key != NO_SIGN_IN_KEY {
                missing.push(format!(
                    "an accounts fact for later other than the recorded no-route error: {words} ({})",
                    posted.key
                ));
                continue;
            }
            let rows: Vec<&str> = end
                .map_or(&posted.detail, |e| &e.detail)
                .iter()
                .filter_map(|d| d.split_once(": ").map(|(head, _)| head))
                .filter(|head| !head.is_empty() && !head.contains(' '))
                .collect();
            later.push(format!(
                "{}{} ×{} ({})",
                if posted.key.is_empty() {
                    &posted.title
                } else {
                    &posted.key
                },
                if posted.key == NO_SIGN_IN_KEY && !rows.is_empty() {
                    format!(" for {}", rows.join(", "))
                } else {
                    String::new()
                },
                end.map_or(1, |e| e.rep.max(1)),
                end.map_or("open", |e| e.how.as_str())
            ));
        } else if end.is_some_and(|e| e.record) {
            records += 1;
        } else {
            missing.push(format!("on the owner's screen: {words}"));
        }
    }
    for n in notes.iter().filter(|n| n.kind == "dropped") {
        lost += n.dropped.max(1);
    }
    if let Some(s) = journal.iter().find(|s| {
        s.line.starts_with("EXCEPTION ")
            || s.line.starts_with("ESCALATED ")
            || s.line.starts_with("NEEDS ")
            || s.line.starts_with("LEFT ")
            || typed_for(&s.line, RULE_LOGIN)
    }) {
        missing.push(format!("`{}`", clip(&s.line, 90)));
    }
    let evidence = format!(
        "{} message(s) since the instance's start: {records} record(s); for later: {}; nothing on \
         the owner's screen, no error, no /login; the journal holds no exception, escalation, need \
         or /login",
        notes.iter().filter(|n| n.kind == "posted").count(),
        if later.is_empty() {
            "none".to_string()
        } else {
            later.join(", ")
        }
    );
    if missing.is_empty() && lost > 0 {
        return Judged {
            verdict: Verdict::Unproven,
            evidence: format!("{evidence}; but the log dropped {lost} line(s) it could not keep"),
        };
    }
    Judged::of(&missing, evidence)
}

/// The rules a harness act in this proof may be typed for: the account move and the limit's
/// resume, and the continue policy's (cruise's) own continuations.
pub const ALLOWED_RULES: [&str; 8] = [
    RULE_ACCOUNT,
    RULE_LIMIT_RESUME,
    RULE_CONTINUE,
    RULE_DONE_CHECK,
    RULE_OPEN_ITEMS,
    RULE_UNCOMMITTED,
    RULE_SUGGESTION,
    RULE_HOST_TURN,
];

/// Claude Code's composer prompt: the glyph its input row opens with.
const COMPOSER: &str = "❯";

/// The text of a harness `send` fenced on the person (`if-human=`) and on the composer's empty
/// row (`if=` the row guard of [`COMPOSER`] alone), where it is no slash command: the turn-end
/// loop's resume or continuation. `None` for any other act.
fn composer_send(line: &str) -> Option<&str> {
    let (head, text) = line.split_once(" -- ")?;
    let fences: Vec<&str> = head.strip_prefix("send ")?.split(' ').collect();
    let empty = format!("if={}", row_guard(COMPOSER));
    (fences.iter().all(|w| w.starts_with("if"))
        && fences.iter().any(|w| w.starts_with("if-human="))
        && fences.contains(&empty.as_str())
        && !text.starts_with('/'))
    .then_some(text)
}

/// Whether a harness `key` act is one Enter fenced on the person and on the composer's row that
/// holds `sent` (Claude Code draws it after [`COMPOSER`] and a no-break space): its `if=` guard,
/// past any indentation and before its row end, is a part of that row's guard (the whole row, a
/// wrapped row of it, or the prefix a long guard keeps) that reaches past the prompt glyph and its
/// space, so no Enter on an empty composer passes. Its only other words are fences (`if-human=`,
/// `if-gen=`).
fn composer_enter(line: &str, sent: &str) -> bool {
    let words: Vec<&str> = line.split(' ').collect();
    let [first, fences @ .., last] = words.as_slice() else {
        return false;
    };
    if *first != "key" || *last != "enter" || !fences.iter().all(|w| w.starts_with("if")) {
        return false;
    }
    if !fences.iter().any(|w| w.starts_with("if-human=")) {
        return false;
    }
    let Some(guard) = fences.iter().find_map(|w| w.strip_prefix("if=")) else {
        return false;
    };
    let Some(mut body) = guard.strip_prefix('^') else {
        return false;
    };
    if let Some(rest) = body.strip_prefix("\\x20{") {
        body = rest
            .split_once('}')
            .filter(|(n, _)| n.parse::<u32>().is_ok())
            .map_or(body, |(_, rest)| rest);
    }
    let body = body.strip_suffix(ROW_END).unwrap_or(body);
    let unwrap = |guard: &str| {
        let g = guard.strip_prefix('^').unwrap_or(guard);
        g.strip_suffix(ROW_END).unwrap_or(g).to_string()
    };
    let held = unwrap(&row_guard(&format!("{COMPOSER}\u{a0}{sent}")));
    // The prompt glyph and its space as the guard spells them (a row's guard trims its trailing
    // space, so a letter holds it and is taken off again).
    let prompt = unwrap(&row_guard(&format!("{COMPOSER}\u{a0}x")));
    let prompt = prompt.strip_suffix('x').unwrap_or(&prompt);
    !prompt.starts_with(body) && held.contains(body)
}

/// The keys: every `TYPED` record is of an [`ALLOWED_RULES`] rule (never the usage probe's,
/// whose `/usage` the tap's figures make undue, nor [`RULE_LOGIN`]); and every input act on the
/// tab's tape is one the allowed rules make: a send into the empty composer, fenced on the person
/// ([`composer_send`]), or the one Enter after it on the composer's row holding its text
/// ([`composer_enter`]). Any other `key` (a digit, an arrow, an Esc, an Enter on a question box's
/// option row, a second Enter), a `paste` or a `turn`, `/login` sent or entered, or `/usage` sent,
/// is a Fail.
#[must_use]
pub fn judge_keys(journal: &[Said], acts: &[Act]) -> Judged {
    let mut missing = Vec::new();
    let typed: Vec<&Said> = journal.iter().filter(|s| is_typed(&s.line)).collect();
    let rule_of = |s: &Said| {
        s.line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("rule="))
            .unwrap_or("")
            .to_string()
    };
    for s in &typed {
        let rule = rule_of(s);
        if !ALLOWED_RULES.contains(&rule.as_str()) {
            missing.push(format!("typed for {rule}: `{}`", clip(&s.line, 70)));
        }
    }
    let typed_acts = inputs(acts);
    let (mut sent, mut sends) = (None::<&str>, 0);
    for a in &typed_acts {
        if is_login_send(&a.line) || is_login_enter(&a.line) || a.line.ends_with(" -- /usage") {
            missing.push(format!(
                "a key that opens or answers a picker: `{}`",
                clip(&a.line, 80)
            ));
        } else if let Some(text) = composer_send(&a.line) {
            sent = Some(text);
            sends += 1;
        } else if sent.is_some_and(|text| composer_enter(&a.line, text)) {
            sent = None;
        } else {
            missing.push(format!(
                "an input act outside the composer's send and its one Enter: `{}`",
                clip(&a.line, 80)
            ));
        }
    }
    let rules: std::collections::BTreeSet<String> = typed.iter().map(|s| rule_of(s)).collect();
    Judged::of(
        &missing,
        format!(
            "{} TYPED record(s), every one of {}; {} input act(s): {sends} fenced send(s) into the \
             empty composer, each entered at most once on the row holding it; no other key, no \
             /login, no /usage",
            typed.len(),
            if rules.is_empty() {
                "none".to_string()
            } else {
                rules.into_iter().collect::<Vec<_>>().join(", ")
            },
            typed_acts.len()
        ),
    )
}

/// The keychain items the owner's own Claude Code login is kept under, un-suffixed: the
/// machine's default login asks for them by these names, a launch whose [`LOGIN_ENV`] took for
/// its login directory's suffixed ones.
const OWNER_ITEMS: [&str; 2] = ["Claude Code", "Claude Code-credentials"];

/// The jail's rules that keep every keychain read from the owner's keychain: the CLI's exec, the
/// security daemon's lookup, and the keychains' folder.
const KEYCHAIN_DENIALS: [&str; 3] = [
    "(deny process-exec (literal \"/usr/bin/security\"))",
    "(deny mach-lookup (global-name \"com.apple.SecurityServer\"))",
    "/Library/Keychains\"))",
];

/// The keychain tripwire (`security.log`, the lab's `security` stand-in's record of each item
/// asked for, with the asker's parent pid) judged by the launch that asked ([`Launched`] by
/// pid): an owner's un-suffixed item ([`OWNER_ITEMS`]) asked for by a launch on a login
/// directory, or by a process no lab launch is the parent of, is a Fail, as is a jail that does
/// not deny the keychain ([`KEYCHAIN_DENIALS`]); the default login's launches ask for the
/// un-suffixed items, which the stand-in answers and the jail keeps from the owner's keychain.
/// Ok where both kinds of launch reached the stand-in; else Unproven.
#[must_use]
pub fn judge_keychain(security_log: &str, launches: &[Launched], jail_sb: &str) -> Judged {
    let mut missing = Vec::new();
    let (mut default_asks, mut own_asks) = (0, 0);
    for l in security_log.lines() {
        let Some(service) = l
            .split_once("service=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(service, _)| service)
        else {
            continue;
        };
        let ppid: Option<i32> = l
            .split_whitespace()
            .find_map(|w| w.strip_prefix("ppid="))
            .and_then(|p| p.parse().ok());
        let by = ppid.and_then(|p| launches.iter().find(|x| x.pid == p));
        let owners = OWNER_ITEMS.contains(&service);
        match by {
            Some(x) if x.login == DEFAULT_LOGIN => {
                if owners {
                    default_asks += 1;
                } else {
                    missing.push(format!("the default login asked for `{service}`"));
                }
            }
            Some(x) => {
                if owners {
                    missing.push(format!(
                        "a launch on {} asked for the owner's `{service}`",
                        clip(&x.login, 40)
                    ));
                } else {
                    own_asks += 1;
                }
            }
            None if owners => missing.push(format!(
                "`{service}` asked for by pid {} that is no lab launch's child",
                ppid.map_or_else(|| "?".to_string(), |p| p.to_string())
            )),
            None => own_asks += 1,
        }
    }
    for rule in KEYCHAIN_DENIALS {
        if !jail_sb.contains(rule) {
            missing.push(format!("the jail lacks `{rule}`"));
        }
    }
    let evidence = format!(
        "{default_asks} un-suffixed ask(s) by the default login's launches, {own_asks} suffixed by \
         the login directories' (the stand-in answered each; the jail denies the keychain)"
    );
    if !missing.is_empty() {
        return Judged::of(&missing, evidence);
    }
    Judged {
        verdict: if default_asks > 0 && own_asks > 0 {
            Verdict::Ok
        } else {
            Verdict::Unproven
        },
        evidence,
    }
}

/// The rows that judge the run's safety and its quiet, whatever its scenarios did.
pub const SAFETY_ROWS: [&str; 8] = [
    "asked", "keys", "procs", "open", "keychain", "egress", "owner", "left",
];

/// The live negative control (`--no-route-seam`): the instance, built without the route seam,
/// reads the stub's route as a route, so the tab is no stored login's and R1's wall moves nothing,
/// for that reason alone. Ok ⇔ P1's turn (the first `main` 200 turn from `start`, the first
/// prompt) carried `oat:lab-a`; the stub gave `oat:lab-a` a 429 within R1's window; aterm.log
/// holds, from `start` through R1's window, the host's `account advice: stay
/// (not-a-stored-login)` or its `account wall not-stored`; the journal holds no account restart
/// and the stub served no turn on `oat:lab-b` or `oat:lab-c`; `rows`' R1 is a Fail (a move the
/// control should not allow would pass it); and each of [`SAFETY_ROWS`] that `rows` holds is Ok,
/// the keychain's alone also Unproven (only the default login launches, so no login directory's
/// launch asks). Else Fail, naming each clause that does not hold.
#[must_use]
pub fn judge_control(ev: &Ev, start: i64, r1: &Leg, rows: &[(&str, Judged)]) -> Judged {
    let mut missing = Vec::new();
    match ev
        .served
        .iter()
        .find(|s| s.t_ms >= start && s.kind == "main" && s.status == 200)
    {
        Some(s) if s.auth == AUTH_A => {}
        Some(s) => missing.push(format!("P1 was served on {}, not {AUTH_A}", s.auth)),
        None => missing.push(format!("P1 was never served on {AUTH_A}")),
    }
    if !ev
        .served
        .iter()
        .any(|s| (r1.from..=r1.to).contains(&s.t_ms) && s.auth == AUTH_A && s.status == 429)
    {
        missing.push(format!("{AUTH_A} was never walled in R1's window"));
    }
    let stay = format!(
        ": account advice: stay ({})",
        StayWhy::NotAStoredLogin.as_str()
    );
    let wall = format!(": account wall {}", Recorded::NotStored.as_str());
    let said = [&stay, &wall]
        .into_iter()
        .find(|needle| !logged(&ev.aterm_log, needle, (start, r1.to)).is_empty());
    if said.is_none() {
        missing.push(format!(
            "aterm.log holds neither `{}` nor `{}` from the first prompt through R1",
            stay.trim_start_matches(": "),
            wall.trim_start_matches(": ")
        ));
    }
    if let Some(s) = any_restart(&ev.journal, (0, i64::MAX)) {
        missing.push(format!("restarted: `{}`", clip(&s.line, 80)));
    }
    if let Some(s) = ev
        .served
        .iter()
        .find(|s| s.auth == AUTH_B || s.auth == AUTH_C)
    {
        missing.push(format!("a turn served on {} ({})", s.auth, s.status));
    }
    match rows.iter().find(|(id, _)| *id == "R1") {
        Some((_, j)) if j.verdict == Verdict::Fail => {}
        Some(_) => missing.push("R1 did not fail".into()),
        None => missing.push("no R1 row".into()),
    }
    for (id, j) in rows.iter().filter(|(id, _)| SAFETY_ROWS.contains(id)) {
        let held =
            j.verdict == Verdict::Ok || (*id == "keychain" && j.verdict == Verdict::Unproven);
        if !held {
            missing.push(format!("the {id} row is {:?}", j.verdict));
        }
    }
    Judged::of(
        &missing,
        format!(
            "R1 failed for the route alone: P1 on {AUTH_A}, {AUTH_A} walled, aterm.log `{}`, no \
             restart, no turn on another row; every safety row holds",
            said.map_or("", |s| s.trim_start_matches(": "))
        ),
    )
}

// ---------------------------------------------------------------------------------------------
// The Claude Code processes' watcher.

/// Looks every 300 ms for the lab's Claude Code processes (the instance's descendants that run
/// the lab's Claude Code binary) and records each one's login, once, as it is first seen.
struct ClaudeWatch {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Launched>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ClaudeWatch {
    fn start(run: &Run) -> ClaudeWatch {
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (instance, bin) = (run.instance_pid, run.claude_bin.clone());
        // aterm relaunches the binary by its resolved path, the lab's launcher by the name it
        // was found at: either is the lab's Claude Code.
        let canon = fs::canonicalize(&bin).ok();
        let (s, k) = (Arc::clone(&stop), Arc::clone(&seen));
        let handle = std::thread::spawn(move || {
            // A pid is looked at again whenever its command line changes (a shell that execs
            // Claude Code keeps its pid), and recorded once it is Claude Code.
            let (mut known, mut looked) = (
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::new(),
            );
            while !s.load(Ordering::SeqCst) {
                let rows = lp::ps_rows();
                let table: Vec<(i32, i32)> = rows.iter().map(|(p, pp, _)| (*p, *pp)).collect();
                let family = crate::procs::descendants_in(&table, instance);
                for (pid, _, command) in &rows {
                    if *pid == instance
                        || !family.contains(pid)
                        || known.contains(pid)
                        || !looked.insert((*pid, command.clone()))
                    {
                        continue;
                    }
                    let Some(args) = u32::try_from(*pid)
                        .ok()
                        .and_then(atpkg::caller_shell::process_args)
                    else {
                        continue;
                    };
                    let is_claude = args.argv.first().is_some_and(|a0| *a0 == bin)
                        || canon.as_ref().is_some_and(|c| {
                            fs::canonicalize(&args.exec_path).is_ok_and(|e| e == *c)
                        });
                    if !is_claude {
                        continue;
                    }
                    known.insert(*pid);
                    let login = args
                        .env_var(LOGIN_ENV)
                        .filter(|v| !v.is_empty())
                        .unwrap_or(DEFAULT_LOGIN)
                        .to_string();
                    k.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(Launched {
                            t_ms: now_ms(),
                            pid: *pid,
                            login,
                            resumed: args.argv.iter().any(|a| a == "--resume"),
                        });
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        ClaudeWatch {
            stop,
            seen,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> Vec<Launched> {
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

impl Drop for ClaudeWatch {
    fn drop(&mut self) {
        self.halt();
    }
}

// ---------------------------------------------------------------------------------------------
// The world and the tab.

/// The paths of the lab's three-row world, every one in the lab's scratch.
struct World {
    /// The roster the instance's sync writes (`cfg/aterm/accounts.toml`).
    roster: PathBuf,
    /// The fleet file beside it, which names the rows.
    fleet: PathBuf,
    /// The instance's harness state (`state/harness/claude-harness`).
    harness: PathBuf,
    /// The machine's default login: the scratch HOME's `.claude`.
    default_login: PathBuf,
    /// The default login's profile: the scratch HOME's `.claude.json`.
    profile: PathBuf,
    /// `lab-b`'s login directory in the accounts store.
    login_b: PathBuf,
    /// `lab-c`'s.
    login_c: PathBuf,
}

impl World {
    fn of(run: &Run) -> Result<World, Fail> {
        let harness = run.state().join("harness").join("claude-harness");
        let store = Store::at(&harness);
        let dir = |label: &str| {
            store.login_dir(label).ok_or_else(|| {
                die(
                    "the accounts store",
                    format!("{label} names no login directory"),
                )
            })
        };
        let home = run.w.join("home");
        Ok(World {
            roster: run.w.join("cfg/aterm").join(ACCOUNTS_FILE),
            fleet: run.w.join("cfg/aterm").join(FLEET_ROSTER_FILE),
            login_b: dir(LABEL_B)?,
            login_c: dir(LABEL_C)?,
            default_login: home.join(".claude"),
            profile: home.join(".claude.json"),
            harness,
        })
    }

    /// Each row's login as a launch names it ([`Launched::login`]).
    fn dirs(&self) -> Vec<(String, String)> {
        vec![
            (LABEL_A.into(), DEFAULT_LOGIN.into()),
            (LABEL_B.into(), self.login_b.to_string_lossy().into_owned()),
            (LABEL_C.into(), self.login_c.to_string_lossy().into_owned()),
        ]
    }

    fn ledger(&self) -> PathBuf {
        Store::at(&self.harness).ledger_path()
    }
}

fn die(what: &str, e: impl std::fmt::Display) -> Fail {
    Fail::die(format!("claude-rotation-proof: {what}: {e}"))
}

/// A live fake Claude login for `label`: its access token the stub reads as `oat:<label>`
/// ([`crate::stub::labelled_oauth_token`]), a fake refresh token, a year to its expiry.
#[must_use]
pub fn live_login(label: &str, now_ms: i64) -> String {
    let login = J::obj([(
        "claudeAiOauth",
        J::obj([
            (
                "accessToken",
                J::s(crate::stub::labelled_oauth_token(label)),
            ),
            (
                "refreshToken",
                J::s(format!("sk-ant-ort01-LABLABLAB-{label}-not-a-token")),
            ),
            ("expiresAt", J::Int(now_ms + 365 * 86_400_000)),
            (
                "scopes",
                J::Arr(vec![J::s("user:inference"), J::s("user:profile")]),
            ),
            ("subscriptionType", J::s("max")),
        ]),
    )]);
    format!("{}\n", login.dump(crate::pyjson::COMPACT))
}

/// Write `label`'s live login into `dir` (`.credentials.json`, owner-only), as Claude Code keeps
/// a login where no keychain answers.
fn put_login(dir: &Path, label: &str) -> Result<(), Fail> {
    fs::create_dir_all(dir).map_err(|e| die(&dir.display().to_string(), e))?;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    let creds = dir.join(".credentials.json");
    crate::sys::put(
        &creds,
        0o600,
        &crate::sys::PutSrc::Bytes(live_login(label, now_ms()).as_bytes()),
    )
    .map_err(|e| die(&creds.display().to_string(), e))
}

/// The default login's profile (`home/.claude.json`, as it stands) naming `email`'s account
/// (`uuid`), as Claude Code writes it once a sign-in read the account's profile.
fn put_profile(world: &World, email: &str, uuid: &str) -> Result<(), Fail> {
    let text = crate::verbs::read_scratch(&world.profile)?;
    let mut config = J::parse(&text).map_err(|e| die("the scratch .claude.json", e))?;
    let mut account = crate::stub::account::composed_oauth_account(now_ms());
    account.set("emailAddress", J::s(email));
    account.set("accountUuid", J::s(uuid));
    config.set("oauthAccount", account);
    atomic_write(
        &world.profile,
        config.dump(crate::pyjson::COMPACT).as_bytes(),
    )
    .map_err(|e| die(&world.profile.display().to_string(), e))
}

/// A sign-in on the default login as `/login` leaves it: `label`'s login in the default login's
/// directory, then its account in the profile. Nothing is pressed anywhere.
fn sign_in_default(world: &World, label: &str, email: &str, uuid: &str) -> Result<(), Fail> {
    put_login(&world.default_login, label)?;
    put_profile(world, email, uuid)
}

/// The address the default login's profile names now.
fn profile_email(world: &World) -> String {
    J::parse(&scratch_text(&world.profile))
        .ok()
        .and_then(|j| {
            j.get("oauthAccount")
                .and_then(|a| a.get("emailAddress"))
                .and_then(J::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// The fleet file the lab lays: the three rows of [`MACHINE`], opted in by name.
#[must_use]
pub fn fleet_text() -> String {
    let mut out = format!(
        "# The lab's fleet: three accounts of one machine.\n{} = \"{}\"\n\n",
        aterm_agent::harness::accounts_sync::OPT_IN_KEY,
        OptIn::Named.as_str()
    );
    for (label, email) in [(LABEL_A, EMAIL_A), (LABEL_B, EMAIL_B), (LABEL_C, EMAIL_C)] {
        out.push_str(&format!(
            "[[account]]\nlabel = \"{label}\"\nmachines = [\"{MACHINE}\"]\nclass = \"dedicated\"\nemail = \"{email}\"\n\n"
        ));
    }
    out
}

/// The headers each credential's answers carry: its five-hour figure `five` and the week's
/// baseline, the windows resetting 2 h and 5 days ahead, all allowed.
fn usage_blocks(five: &str) -> J {
    let now = now_secs();
    J::obj([(
        "on200",
        J::obj([
            ("anthropic-ratelimit-unified-status", J::s("allowed")),
            ("anthropic-ratelimit-unified-5h-status", J::s("allowed")),
            ("anthropic-ratelimit-unified-5h-utilization", J::s(five)),
            (
                "anthropic-ratelimit-unified-5h-reset",
                J::Int(now + 2 * 3600),
            ),
            ("anthropic-ratelimit-unified-7d-status", J::s("allowed")),
            (
                "anthropic-ratelimit-unified-7d-utilization",
                J::s(BASELINE_7D),
            ),
            (
                "anthropic-ratelimit-unified-7d-reset",
                J::Int(now + 5 * 86_400),
            ),
            (
                "anthropic-ratelimit-unified-representative-claim",
                J::s("five_hour"),
            ),
        ]),
    )])
}

/// Lay the three-row world in the lab's scratch, nowhere else: settings with no `apiKeyHelper`,
/// no proxy and the stub's own base URL (the route the seam admits); the default login holding
/// `lab-a`'s login, its profile naming `lab-a`'s account; `lab-b`'s login in its own directory;
/// the fleet file beside the roster, and no roster (the instance's sync writes it); each
/// credential's baseline figures.
fn lay_world(run: &Run, world: &World) -> Result<(), Fail> {
    for p in [
        &world.roster,
        &world.fleet,
        &world.default_login,
        &world.profile,
        &world.login_b,
        &world.login_c,
    ] {
        if !p.starts_with(&run.w) {
            return Err(die("refusing to write outside the scratch", p.display()));
        }
    }
    lp::no_mailbox(run)?;
    let mut env = J::Obj(Vec::new());
    for k in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        env.set(k, J::Null);
    }
    env.set("ANTHROPIC_BASE_URL", J::s(crate::lab::stub_base_url(run)));
    env.set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", J::s("1"));
    crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null), ("env", env)]))?;
    let config = crate::verbs::read_scratch(&world.default_login.join(".claude.json"))?;
    let mut config = J::parse(&config).map_err(|e| die("the scratch .claude.json", e))?;
    config.remove("oauthAccount");
    atomic_write(
        &world.profile,
        config.dump(crate::pyjson::COMPACT).as_bytes(),
    )
    .map_err(|e| die(&world.profile.display().to_string(), e))?;
    sign_in_default(world, LABEL_A, EMAIL_A, UUID_A)?;
    put_login(&world.login_b, LABEL_B)?;
    if let Some(logins) = world.login_b.parent() {
        // The store's own logins folder is owner-only, as the store makes it.
        let _ = fs::set_permissions(logins, fs::Permissions::from_mode(0o700));
    }
    let fleet = fleet_text();
    let parsed = FleetRoster::from_toml(&fleet, "the lab's fleet")
        .map_err(|e| die("the fleet the lab writes", e))?;
    if parsed.accounts_for_machine(MACHINE).len() != 3 {
        return Err(die(
            "the fleet the lab writes",
            "it lists no three rows of its machine",
        ));
    }
    if world.roster.exists() {
        // The sync writes the roster from the fleet; one laid before it would be a hand's.
        fs::remove_file(&world.roster).map_err(|e| die(&world.roster.display().to_string(), e))?;
    }
    atomic_write(&world.fleet, fleet.as_bytes())
        .map_err(|e| die(&world.fleet.display().to_string(), e))?;
    for auth in [AUTH_A, AUTH_B, AUTH_C] {
        crate::verbs::write_headers_for(&run.w, auth, usage_blocks(BASELINE_5H))?;
    }
    Ok(())
}

/// What a roster the sync wrote must hold: `lab-a` the default login's row, `lab-b` and
/// `lab-c` on their login directories of the store (`b`, `c`). `None` where it does; else what
/// it lacks.
#[must_use]
pub fn roster_lacks(text: &str, (b, c): (&Path, &Path)) -> Option<String> {
    let roster = match Roster::parse(text) {
        Ok(r) => r,
        Err(e) => return Some(format!("the roster does not parse ({e})")),
    };
    if roster.row_for_login(None).map(|r| r.label.as_str()) != Some(LABEL_A) {
        return Some(format!("{LABEL_A} is not the default login's row"));
    }
    for (label, dir) in [(LABEL_B, b), (LABEL_C, c)] {
        let want = dir.to_string_lossy();
        match roster.row(label).and_then(|r| r.dir.as_deref()) {
            Some(d) if d == want => {}
            other => {
                return Some(format!(
                    "{label} names {}, not {want}",
                    other.unwrap_or("no login directory")
                ));
            }
        }
    }
    None
}

/// The tab's launcher: the installed Claude Code by path on the machine's default login (no
/// [`LOGIN_ENV`]), its route from the scratch settings alone, aterm's usage tap on its line as
/// the managed twin's launch would carry it, and every `open` and `$BROWSER` the lab's recorder.
fn launcher(run: &Run) -> Result<String, Fail> {
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    let tap =
        aterm_agent::harness::tap::settings_value(Path::new(&run.aterm)).ok_or_else(|| {
            die(
                "the usage tap",
                format!("{} cannot be named in it", run.aterm),
            )
        })?;
    let w = &run.w;
    Ok(format!(
        "#!/bin/sh\nclear\nexec env -u CLAUDE_CONFIG_DIR -u {LOGIN_ENV} -u ANTHROPIC_BASE_URL -u HTTP_PROXY \
         -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \\\n  PATH=\"{}:$PATH\" \
         DISABLE_AUTOUPDATER=1 \\\n  LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} \\\n  {} --settings {}\n",
        run.run.join("shim").display(),
        q(&w.join("security.log")),
        q(&w.join("open.log")),
        q(&run.run.join("shim/open")),
        shell_quote(&run.claude_bin),
        shell_quote(&tap)
    ))
}

/// The line typed into the tab's shell: the lab's `security` and `open` stand-ins first on its
/// PATH and their logs in its environment, for the Claude Code it starts and every one aterm
/// starts in it after, then the launcher.
fn start_line(run: &Run, script: &Path) -> String {
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    format!(
        "export PATH={}:\"$PATH\" LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} DISABLE_AUTOUPDATER=1; sh {}",
        q(&run.run.join("shim")),
        q(&run.w.join("security.log")),
        q(&run.w.join("open.log")),
        q(&run.run.join("shim/open")),
        q(script)
    )
}

/// Supervise tab `sid` under cruise with a task, launch Claude Code in it on the default login
/// and wait for its empty composer.
fn start_tab(run: &Run, door: &Door<'_>, sid: &str) -> Result<(), Fail> {
    for args in [
        &["meta", "set", "autopilot", "cruise"][..],
        &["task", "mark", "working", "from=ctl"],
    ] {
        let r = on(door, sid, args)?;
        if r.code != 0 {
            return Err(die(&args.join(" "), r.stderr.trim()));
        }
    }
    let script = run.run.join("rotation-launch.sh");
    atomic_write(&script, launcher(run)?.as_bytes())
        .map_err(|e| die(&script.display().to_string(), e))?;
    let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
    on(door, sid, &["send", &start_line(run, &script)])?;
    on(door, sid, &["key", "enter"])?;
    if let Some(why) = crate::forever::empty_composer(door, sid, "claude")?.why("Claude Code") {
        return Err(die(&format!("tab {sid}"), why));
    }
    Ok(())
}

/// The local midnight of `secs`' day, ms since the epoch.
fn day_start_ms(secs: i64) -> i64 {
    let (_, _, _, h, m, s) = crate::sys::local_time(secs);
    (secs - i64::from(h * 3600 + m * 60 + s)) * 1000
}

/// What the scenarios left for the judges.
#[derive(Debug, Default)]
struct Marks {
    sid: String,
    /// The first prompt's Enter, ms.
    start: i64,
    /// The local midnight the stub log's stamps count from, ms.
    day: i64,
    r1: Leg,
    r3: Leg,
    r4: Leg,
    r5a: Leg,
    r5b: Leg,
    r2: Leg,
    /// The lab's own prompts, with when each was entered.
    prompts: Vec<(i64, String)>,
    /// Why the scenarios stopped short, where they did.
    blocked: Option<String>,
    /// Under `--no-route-seam`: the scenarios after R1 are not run.
    control: bool,
}

/// The run's driver: the door, the world, and the stub log's day.
struct Drive<'a> {
    run: &'a Run,
    door: Door<'a>,
    world: &'a World,
    sid: String,
    day: i64,
    grace: Duration,
}

impl Drive<'_> {
    fn served(&self) -> Vec<Served> {
        served_of(&scratch_text(&self.run.w.join("stub.log")), self.day)
    }

    fn journal(&self) -> Vec<Said> {
        read_journal(self.run, &self.sid)
    }

    /// When the accounts' ledger books `kind` for `label` at or after `from` (ms) with a row
    /// `why` admits.
    fn booked(
        &self,
        label: &str,
        kind: &str,
        from: i64,
        why: impl Fn(&Row) -> bool,
    ) -> Option<i64> {
        booked_after(
            &rows_of(&scratch_text(&self.world.ledger())),
            label,
            kind,
            from / 1000 - 1,
            why,
        )
    }

    /// `aterm harness accounts --json` as the instance's own CLI prints it now.
    fn listing(&self) -> String {
        match self.door.harness(&["accounts", "--json"]) {
            Ok(r) => r.stdout,
            Err(why) => why,
        }
    }

    fn wait<T>(
        &self,
        what: &str,
        cap: Duration,
        found: impl FnMut() -> Option<T>,
    ) -> Result<Result<T, String>, Fail> {
        lp::hang_wait_capped(self.run, &self.door, &self.sid, what, cap, found)
    }

    /// A scheduled wait: `found` by `until` (ms) and a minute more, whatever moves or not.
    fn wait_until<T>(
        &self,
        what: &str,
        until: i64,
        mut found: impl FnMut() -> Option<T>,
    ) -> Result<Result<T, String>, Fail> {
        loop {
            if let Some(t) = found() {
                return Ok(Ok(t));
            }
            let deadline = until + i64::try_from(HANG.as_millis()).unwrap_or(60_000);
            if now_ms() >= deadline {
                return Ok(Err(format!(
                    "{what}: not seen by {} s past its instant",
                    HANG.as_secs()
                )));
            }
            stop::sleep(Duration::from_millis(500))?;
        }
    }

    /// Type `prompt` at the tab's empty composer, fenced; `Ok(None)` where it went in.
    fn prompt(&self, m: &mut Marks, prompt: &str) -> Result<Option<String>, Fail> {
        if let Some(why) =
            crate::forever::empty_composer(&self.door, &self.sid, "claude")?.why("Claude Code")
        {
            return Ok(Some(why));
        }
        match lp::first_prompt(self.run, &self.door, &self.sid, prompt)? {
            Ok(()) => {
                m.prompts.push((now_ms(), prompt.to_string()));
                println!("  typed `{prompt}`");
                Ok(None)
            }
            Err(why) => Ok(Some(why)),
        }
    }

    /// The first 200 `main` turn served at or after `at`: its credential, and its history read
    /// off the stub's newest turn dumps.
    fn served_after(
        &self,
        what: &str,
        at: i64,
    ) -> Result<Result<(String, Vec<String>), String>, Fail> {
        let found = self.wait(what, Duration::from_secs(300), || {
            self.served()
                .into_iter()
                .find(|s| s.t_ms >= at && s.kind == "main" && s.status == 200)
        })?;
        let turn = match found {
            Ok(t) => t,
            Err(why) => return Ok(Err(why)),
        };
        let dumps = crate::stub::turn_dumps(&self.run.w.join("dump"));
        let history = dumps
            .iter()
            .take(8)
            .filter_map(|(_, p)| fs::read(p).ok())
            .map(|b| crate::stub::message_users(&b))
            .find(|users| {
                users.last().is_some_and(|u| {
                    crate::util::one_line(u, 70).starts_with(turn.last_user.trim_end_matches('…'))
                })
            })
            .unwrap_or_default();
        println!(
            "  {what}: {} served it ({} user texts in its history)",
            turn.auth,
            history.len()
        );
        Ok(Ok((turn.auth, history)))
    }

    /// [`Self::served_after`], whose turn must carry `auth`.
    fn served_turn(
        &self,
        what: &str,
        auth: &str,
        at: i64,
    ) -> Result<Result<Vec<String>, String>, Fail> {
        Ok(match self.served_after(what, at)? {
            Ok((on, history)) if on == auth => Ok(history),
            Ok((on, _)) => Err(format!("{what}: served under {on}, not {auth}")),
            Err(why) => Err(why),
        })
    }

    /// Let a move's outcome land before the next condition is set: wait for the ledger's
    /// `proven` row of `label` at or after `at` (ms). A wait that ends without it is said and the
    /// run goes on (the judge's `proven` clause decides).
    fn settle(&self, label: &str, at: i64) -> Result<(), Fail> {
        let said = self.wait(
            &format!("the ledger's record that {label} served"),
            UPKEEP_LOOK,
            || self.booked(label, "proven", at, |_| true),
        )?;
        match said {
            Ok(_) => println!("  the ledger booked {label} proven"),
            Err(why) => println!("  ({why})"),
        }
        Ok(())
    }

    /// A restart of [`RULE_ACCOUNT`] onto `to` at or after `from`.
    fn moved(&self, to: &str, from: i64) -> Option<i64> {
        restarted(&self.journal(), to, (from, i64::MAX)).map(|s| s.t_ms)
    }

    /// A 429 the stub gave `auth` at or after `from`.
    fn walled(&self, auth: &str, from: i64) -> bool {
        self.served()
            .iter()
            .any(|s| s.t_ms >= from && s.auth == auth && s.status == 429)
    }

    /// Type `prompt` where the composer stands empty; a prompt that cannot go in is no stop where
    /// the harness's own turn met the condition (`met`).
    fn meet(
        &self,
        m: &mut Marks,
        prompt: &str,
        met: impl Fn(&Self) -> bool,
    ) -> Result<Option<String>, Fail> {
        match self.prompt(m, prompt)? {
            None => Ok(None),
            Some(why) if met(self) => {
                println!("  ({prompt} not typed: {why}; the harness's own turn met it)");
                Ok(None)
            }
            Some(why) => Ok(Some(why)),
        }
    }

    /// R5a and R5b's step: the default login made `label`'s, as `/login` leaves it; the sync's
    /// displacement of `from` waited for; `prompt` typed and its turn's credential and history
    /// kept, with the instance's listing before and after, in `leg`. `Ok(false)` where the run
    /// cannot go on.
    fn displace(
        &self,
        m: &mut Marks,
        leg_of: fn(&mut Marks) -> &mut Leg,
        (from, label, email, uuid): (&str, &str, &str, &str),
        prompt: &str,
    ) -> Result<bool, Fail> {
        let before = self.listing();
        let start = now_ms();
        {
            let leg = leg_of(m);
            leg.before = before;
            leg.from = start;
        }
        sign_in_default(self.world, label, email, uuid)?;
        println!(
            "  the default login holds {label}'s account now (its login, then its profile, as /login writes them)"
        );
        let shown = self.wait_until(
            &format!("the sync's displacement of {from} onto {label}"),
            start + UPKEEP_EVERY_MS,
            || self.booked(from, "displaced", start, |r| r.to == label),
        )?;
        if let Err(why) = shown {
            leg_of(m).blocked = Some(why);
            return Ok(false);
        }
        println!(
            "  the ledger booked {from} displaced onto {label} (the profile names {})",
            profile_email(self.world)
        );
        let at = now_ms();
        if let Some(why) = self.prompt(m, prompt)? {
            leg_of(m).blocked = Some(format!("{}: {why}", clip(prompt, 20)));
            return Ok(false);
        }
        leg_of(m).asked = at;
        match self.served_after("its next turn", at)? {
            Ok((_, history)) => leg_of(m).history = history,
            Err(why) => {
                leg_of(m).blocked = Some(why);
                return Ok(false);
            }
        }
        leg_of(m).listing = self.listing();
        Ok(true)
    }
}

/// Run R1, R3, R4, R5a, R5b and R2 on one tab ([`Marks`]); a scenario that cannot go on says why.
fn scenarios(d: &mut Drive<'_>, m: &mut Marks) -> Result<(), Fail> {
    let (run, world) = (d.run, d.world);
    d.sid = crate::verbs::sid(&d.door);
    m.sid.clone_from(&d.sid);
    let synced = d.wait("the roster the fleet sync writes", UPKEEP_LOOK, || {
        let text = scratch_text(&world.roster);
        (!text.is_empty() && roster_lacks(&text, (&world.login_b, &world.login_c)).is_none())
            .then_some(())
    })?;
    if let Err(why) = synced {
        let text = scratch_text(&world.roster);
        m.blocked = Some(format!(
            "{why}{}",
            if text.is_empty() {
                String::new()
            } else {
                format!(
                    " ({})",
                    roster_lacks(&text, (&world.login_b, &world.login_c)).unwrap_or_default()
                )
            }
        ));
        return Ok(());
    }
    println!(
        "the instance's sync wrote the roster: {LABEL_A} the default login, {LABEL_B} and {LABEL_C} on their own"
    );
    start_tab(run, &d.door, &d.sid)?;
    m.start = now_ms();
    if let Some(why) = d.prompt(m, P1)? {
        m.blocked = Some(format!("P1: {why}"));
        return Ok(());
    }
    if let Err(why) = d.served_turn("P1's turn", AUTH_A, m.start)? {
        m.blocked = Some(format!(
            "P1 was not served on {AUTH_A}: {why} (the stub's answers: {})",
            d.served()
                .iter()
                .map(|s| format!("{} {}", s.status, s.auth))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return Ok(());
    }

    // R1: lab-a's wall.
    let reset_a = now_secs() + i64::try_from(R1_RESET.as_secs()).unwrap_or(180);
    m.r1.from = now_ms();
    m.r1.reset = reset_a;
    crate::verbs::set_auth_mode(run, AUTH_A, &format!("limit:five_hour:{reset_a}"))?;
    println!("(R1) {AUTH_A} at its five-hour limit until {reset_a}");
    let from = m.r1.from;
    if let Some(why) = d.meet(m, P2, |d| d.walled(AUTH_A, from))? {
        m.r1.blocked = Some(format!("P2: {why}"));
        return Ok(());
    }
    let cap = d.grace + Duration::from_secs(240);
    let at = match d.wait("R1's move onto lab-b", cap, || d.moved(LABEL_B, from))? {
        Ok(at) => at,
        Err(why) => {
            m.r1.blocked = Some(why);
            return Ok(());
        }
    };
    println!(
        "(R1) moved onto {LABEL_B} {:.0} s after the wall was set",
        (at - from) as f64 / 1000.0
    );
    match d.served_turn("R1's next turn", AUTH_B, at)? {
        Ok(h) => m.r1.history = h,
        Err(why) => {
            m.r1.blocked = Some(why);
            return Ok(());
        }
    }
    d.settle(LABEL_B, at)?;
    if m.control {
        return Ok(());
    }

    // R3: lab-b's wall while lab-a's stands.
    let reset_b = reset_a + RESET_GRACE_S + i64::try_from(R3_PAST_A.as_secs()).unwrap_or(150);
    m.r3.from = now_ms();
    m.r1.to = m.r3.from;
    m.r3.reset = reset_b;
    crate::verbs::set_auth_mode(run, AUTH_B, &format!("limit:five_hour:{reset_b}"))?;
    println!(
        "(R3) {AUTH_B} at its five-hour limit until {reset_b}; {AUTH_A}'s stands until {reset_a}"
    );
    let from = m.r3.from;
    if let Some(why) = d.meet(m, P3, |d| d.walled(AUTH_B, from))? {
        m.r3.blocked = Some(format!("P3: {why}"));
        return Ok(());
    }
    let log = lp::aterm_log(run);
    let until = match d.wait(
        "R3's wait on lab-b",
        d.grace + Duration::from_secs(240),
        || advice_until(&scratch_text(&log), LABEL_B, from),
    )? {
        Ok(u) => u,
        Err(why) => {
            m.r3.blocked = Some(why);
            return Ok(());
        }
    };
    println!(
        "(R3) the host waits on {LABEL_B} until {until} ({} s from now, {LABEL_A}'s reset {reset_a})",
        until - now_secs()
    );
    let at = match d.wait_until(
        "R3's move back onto lab-a",
        until * 1000 + UPKEEP_EVERY_MS,
        || d.moved(LABEL_A, from),
    )? {
        Ok(at) => at,
        Err(why) => {
            m.r3.blocked = Some(why);
            return Ok(());
        }
    };
    println!(
        "(R3) moved back onto {LABEL_A} {:.0} s past {until}",
        (at - until * 1000) as f64 / 1000.0
    );
    match d.served_turn("R3's next turn", AUTH_A, at)? {
        Ok(h) => m.r3.history = h,
        Err(why) => {
            m.r3.blocked = Some(why);
            return Ok(());
        }
    }
    d.settle(LABEL_A, at)?;

    // R4: lab-a's wall again, lab-b's login gone.
    let gone = &world.login_b;
    if !gone.starts_with(&run.w) || gone.file_name().and_then(|n| n.to_str()) != Some(LABEL_B) {
        return Err(die("refusing to remove", gone.display()));
    }
    fs::remove_dir_all(gone).map_err(|e| die(&gone.display().to_string(), e))?;
    let reset = now_secs() + i64::try_from(R4_RESET.as_secs()).unwrap_or(180);
    m.r4.from = now_ms();
    m.r3.to = m.r4.from;
    m.r4.reset = reset;
    crate::verbs::set_auth_mode(run, AUTH_B, "off")?;
    crate::verbs::set_auth_mode(run, AUTH_A, &format!("limit:five_hour:{reset}"))?;
    println!("(R4) {LABEL_B}'s login directory removed; {AUTH_A} at its limit until {reset}");
    let from = m.r4.from;
    if let Some(why) = d.meet(m, P4, |d| d.walled(AUTH_A, from))? {
        m.r4.blocked = Some(format!("P4: {why}"));
        return Ok(());
    }
    let opens = reset * 1000 - 5_000;
    let look = i64::try_from(r4_look().as_millis()).unwrap_or(600_000);
    let resumed = d.wait_until(
        "R4's resumed turn at lab-a's reset",
        reset * 1000 + look,
        || {
            d.served()
                .into_iter()
                .find(|s| s.t_ms >= opens && s.kind == "main" && s.status == 200)
                .map(|s| s.t_ms)
        },
    )?;
    let at = match resumed {
        Ok(at) => at,
        Err(why) => {
            m.r4.blocked = Some(why);
            return Ok(());
        }
    };
    println!(
        "(R4) a turn served {:.0} s past {LABEL_A}'s reset",
        (at - reset * 1000) as f64 / 1000.0
    );
    match d.served_turn("R4's resumed turn", AUTH_A, opens)? {
        Ok(h) => m.r4.history = h,
        Err(why) => {
            m.r4.blocked = Some(why);
            return Ok(());
        }
    }
    let tried = d.wait("R4's automatic sign-in of lab-b", UPKEEP_LOOK, || {
        d.booked(LABEL_B, "sign-in", from, |r| r.why.starts_with("auto "))
    })?;
    match tried {
        Ok(_) => println!("(R4) the ledger booked an automatic sign-in of {LABEL_B}"),
        Err(why) => println!("  ({why})"),
    }

    // R5a: the default login made lab-b's (its limit over), as /login makes it.
    let open_b = reset_b + RESET_GRACE_S + 5;
    let left = open_b - now_secs();
    if left > 0 {
        println!("(R5a) {left} s until {LABEL_B}'s limit and its grace have passed");
        stop::sleep(Duration::from_secs(u64::try_from(left).unwrap_or(0)))?;
    }
    println!("(R5a) a sign-in on the default login as {LABEL_B}");
    m.r4.to = now_ms();
    if !d.displace(m, |m| &mut m.r5a, (LABEL_A, LABEL_B, EMAIL_B, UUID_B), P5)? {
        return Ok(());
    }
    let from = m.r5a.from;
    let tried = d.wait(
        "the automatic sign-in of the displaced lab-a",
        UPKEEP_LOOK,
        || d.booked(LABEL_A, "sign-in", from, |r| r.why.starts_with("auto ")),
    )?;
    match tried {
        Ok(_) => {
            println!("(R5a) the ledger booked an automatic sign-in of the displaced {LABEL_A}")
        }
        Err(why) => println!("  ({why})"),
    }

    // R5b: the default login made lab-a's again: the owner's second incident.
    println!("(R5b) a sign-in on the default login as {LABEL_A} again");
    m.r5a.to = now_ms();
    if !d.displace(m, |m| &mut m.r5b, (LABEL_B, LABEL_A, EMAIL_A, UUID_A), P6)? {
        return Ok(());
    }

    // R2: lab-a high, lab-c's login made.
    put_login(&world.login_c, LABEL_C)?;
    crate::verbs::write_headers_for(&run.w, AUTH_A, usage_blocks(HIGH_5H))?;
    m.r2.from = now_ms();
    m.r5b.to = m.r2.from;
    println!("(R2) {LABEL_C}'s login made; {AUTH_A} answers at {HIGH_5H} of its five-hour window");
    m.r2.asked = now_ms();
    if let Some(why) = d.prompt(m, P7)? {
        m.r2.blocked = Some(format!("P7: {why}"));
        return Ok(());
    }
    let from = m.r2.from;
    let at = match d.wait(
        "R2's move onto lab-c ahead of a wall",
        d.grace + Duration::from_secs(240),
        || d.moved(LABEL_C, from),
    )? {
        Ok(at) => at,
        Err(why) => {
            m.r2.blocked = Some(why);
            return Ok(());
        }
    };
    println!(
        "(R2) moved onto {LABEL_C} {:.0} s after the high figure",
        (at - from) as f64 / 1000.0
    );
    // The move came at P7's turn end, which left nothing to resume: the next turn is the lab's.
    if let Some(why) = d.prompt(m, P8)? {
        m.r2.blocked = Some(format!("P8: {why}"));
        return Ok(());
    }
    match d.served_turn("R2's next turn", AUTH_C, at)? {
        Ok(h) => m.r2.history = h,
        Err(why) => {
            m.r2.blocked = Some(why);
            return Ok(());
        }
    }
    d.settle(LABEL_C, at)
}

/// The text evidence the judges read, kept before `down` removes the scratch.
#[derive(Debug, Default)]
struct Kept {
    journal: String,
    ledger: String,
    aterm_log: String,
    stub_log: String,
    drive_ledger: String,
    messages: String,
    acts: String,
    sockets: String,
    egress: String,
    jail_sb: String,
    security: String,
    roster: String,
    fleet: String,
    profile: String,
}

impl Kept {
    fn read(run: &Run, world: &World, sid: &str) -> Kept {
        let text = |p: &Path| scratch_text(p);
        Kept {
            journal: text(&lp::journal_path(run, sid)),
            ledger: text(&world.ledger()),
            aterm_log: text(&lp::aterm_log(run)),
            stub_log: text(&run.w.join("stub.log")),
            drive_ledger: text(&world.harness.join("upgrade").join("ledger.jsonl")),
            messages: text(&run.state().join("logs").join("messages.log")),
            acts: lp::tape_acts(&run.state().join("drive"), sid),
            sockets: text(&run.run.join("sockets.log")),
            egress: text(&run.run.join("egress.log")),
            jail_sb: text(&run.run.join("jail.sb")),
            security: text(&run.w.join("security.log")),
            roster: text(&world.roster),
            fleet: text(&world.fleet),
            profile: text(&world.profile),
        }
    }

    fn save(&self, to: &Path, launches: &str, marks: &Marks) {
        for (name, text) in [
            ("rotation-journal.jsonl", &self.journal),
            ("rotation-ledger.jsonl", &self.ledger),
            ("rotation-stub.log", &self.stub_log),
            ("rotation-drive-ledger.jsonl", &self.drive_ledger),
            ("rotation-acts.txt", &self.acts),
            ("rotation-security.log", &self.security),
            ("rotation-roster.toml", &self.roster),
            ("rotation-fleet.toml", &self.fleet),
            ("rotation-profile.json", &self.profile),
            ("rotation-launches.txt", &launches.to_string()),
            ("rotation-marks.txt", &format!("{marks:#?}\n")),
        ] {
            let _ = fs::write(to.join(name), text);
        }
    }
}

/// A scenario's row: its judge's verdict where it was reached, a Fail naming where it stopped.
fn leg_row(leg: &Leg, judged: impl FnOnce(&Leg) -> Judged, blocked: &str) -> Judged {
    if leg.from == 0 {
        return Judged::not_reached(blocked);
    }
    let j = judged(leg);
    match &leg.blocked {
        Some(why) => Judged {
            verdict: Verdict::Fail,
            evidence: format!("{}; stopped: {why}", j.evidence),
        },
        None => j,
    }
}

/// The evidence the judges read, from the kept text.
fn ev_of(kept: &Kept, m: &Marks, launches: &[Launched], dirs: Vec<(String, String)>) -> Ev {
    Ev {
        journal: journal_of(&kept.journal),
        ledger: rows_of(&kept.ledger),
        aterm_log: kept.aterm_log.clone(),
        served: served_of(&kept.stub_log, m.day),
        launches: launches.to_vec(),
        drive_ledger: kept.drive_ledger.clone(),
        dirs,
        sid: m.sid.clone(),
    }
}

/// One verdict row per judge, in the table's order (the control's own row apart, which reads
/// these: [`judge_control`]).
fn judge_run(
    ev: &Ev,
    kept: &Kept,
    m: &Marks,
    launches: &[Launched],
    (seen, witness, witness_live): (&lp::Seen, &crate::recorder::Witness, bool),
) -> Vec<(&'static str, Judged)> {
    let end = now_ms();
    let close = |leg: &Leg| Leg {
        to: if leg.to > 0 { leg.to } else { end },
        ..leg.clone()
    };
    let blocked = |leg: &Leg| {
        leg.blocked
            .clone()
            .or_else(|| m.blocked.clone())
            .unwrap_or_else(|| "the run stopped before it".into())
    };
    let mut rows: Vec<(&str, Judged)> = Vec::new();
    rows.push((
        "R1",
        leg_row(&close(&m.r1), |l| judge_r1(ev, l), &blocked(&m.r1)),
    ));
    if m.control {
        for id in ["R3", "R4", "R5a", "R5b", "R2"] {
            rows.push((
                id,
                Judged {
                    verdict: Verdict::Unproven,
                    evidence: "not run: --no-route-seam stops after R1, which the later scenarios chain on"
                        .into(),
                },
            ));
        }
    } else {
        rows.push((
            "R3",
            leg_row(
                &close(&m.r3),
                |l| judge_r3(ev, l, m.r1.reset),
                &blocked(&m.r3),
            ),
        ));
        rows.push((
            "R4",
            leg_row(&close(&m.r4), |l| judge_r4(ev, l), &blocked(&m.r4)),
        ));
        let r5a = Displacement {
            from: LABEL_A,
            to: LABEL_B,
            auth: AUTH_B,
            prompts: &[P1, P5],
        };
        rows.push((
            "R5a",
            leg_row(&close(&m.r5a), |l| judge_r5(ev, l, r5a), &blocked(&m.r5a)),
        ));
        let r5b = Displacement {
            from: LABEL_B,
            to: LABEL_A,
            auth: AUTH_A,
            prompts: &[P1, P6],
        };
        rows.push((
            "R5b",
            leg_row(&close(&m.r5b), |l| judge_r5(ev, l, r5b), &blocked(&m.r5b)),
        ));
        rows.push((
            "R2",
            leg_row(&close(&m.r2), |l| judge_r2(ev, l), &blocked(&m.r2)),
        ));
    }
    rows.push(("stub", judge_stub(&ev.served)));
    rows.push(("asked", judge_asked(&ev.journal, &notes_of(&kept.messages))));
    rows.push(("keys", judge_keys(&ev.journal, &acts_of(&kept.acts))));
    rows.push((
        "procs",
        judge_procs(&seen.sightings, seen.control, seen.looks),
    ));
    rows.push((
        "open",
        Judged {
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
        },
    ));
    rows.push((
        "keychain",
        judge_keychain(&kept.security, launches, &kept.jail_sb),
    ));
    let mut egress = judge_egress(&kept.sockets, &kept.egress);
    if !witness_live && egress.verdict == Verdict::Ok {
        egress.verdict = Verdict::Unproven;
        egress
            .evidence
            .push_str("; the witness was not running at the end");
    }
    rows.push(("egress", egress));
    rows.push((
        "owner",
        judge_owner(&kept.jail_sb, &crate::paths::paths().guarded_homes()),
    ));
    rows
}

/// The verb's exit: 0 where no row is a Fail, else 1; under `--no-route-seam` (`control`), whose
/// R1 must fail, 0 where the control row ([`judge_control`]) is Ok, else 1.
#[must_use]
pub fn verdict_code(control: bool, rows: &[(&str, Judged)]) -> i32 {
    let held = if control {
        rows.iter()
            .any(|(id, j)| *id == "control" && j.verdict == Verdict::Ok)
    } else {
        rows.iter().all(|(_, j)| j.verdict != Verdict::Fail)
    };
    i32::from(!held)
}

/// Print the verdict table (and keep it in `kept`), and return [`verdict_code`].
fn print_verdicts(run: &Run, m: &Marks, rows: &[(&str, Judged)], kept: Option<&Path>) -> i32 {
    println!();
    println!(
        "claude-rotation-proof: Claude Code {} · tab {} · rows {LABEL_A} (default login) {LABEL_B} {LABEL_C}{}",
        run.claude_ver,
        m.sid,
        if m.control {
            " · CONTROL --no-route-seam (the instance reads the stub's route as a route)"
        } else {
            ""
        }
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
    let code = verdict_code(m.control, rows);
    if m.control {
        println!(
            "claude-rotation-proof: CONTROL {} ({} checks; R1 must FAIL, the control row must PASS)",
            if code == 0 { "HELD" } else { "BROKEN" },
            rows.len()
        );
        return code;
    }
    if failed == 0 {
        println!(
            "claude-rotation-proof: PASS ({} checks{})",
            rows.len(),
            if unproven > 0 {
                format!(", {unproven} of them unproven")
            } else {
                String::new()
            }
        );
    } else {
        println!(
            "claude-rotation-proof: FAIL ({failed} of {} checks)",
            rows.len()
        );
    }
    code
}

/// `claude-rotation-proof [tag] [--keep]` (and the live negative control's `--no-route-seam`,
/// which brings the lab up without the route seam, so the instance reads the stub's route as
/// no stored login and R1 must FAIL on its move, its `control` row judging that it failed for that
/// reason alone, SPECS §21). Under `--keep` the lab is left up and the table judged off its live
/// scratch, the `left` row apart.
pub fn cmd_claude_rotation_proof(args: &[String]) -> Res {
    let (mut tag, mut keep, mut control) = ("rotation-proof".to_string(), false, false);
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            "--no-route-seam" => control = true,
            _ if a.starts_with('-') => {
                return Err(Fail::usage(format!("claude-rotation-proof: unknown {a}")));
            }
            _ => tag.clone_from(a),
        }
    }
    if !crate::paths::valid_tag(&tag) {
        return Err(Fail::die(format!("bad tag '{tag}'")));
    }
    let claude = crate::lab::find_claude().map_err(|e| {
        Fail::die(format!(
            "claude-rotation-proof: no Claude Code to run ({e}): install it (aterm pkg install claude) or name one with LAB_CLAUDE=/path/to/claude"
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
        route_seam: !control,
        ..from_env
    };
    let run = crate::lab::up(&tag, &o).map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            Fail::die(format!(
                "claude-rotation-proof: up failed: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    println!(
        "claude-rotation-proof: Claude Code {} ({claude}) on the stub {} in lab '{tag}'{}",
        run.claude_ver,
        crate::lab::stub_base_url(&run),
        if control {
            " · CONTROL: no route seam"
        } else {
            ""
        }
    );
    let world = World::of(&run);
    let config = run.w.join("cfg/aterm/aterm.toml");
    let (cfg, _) = SupervisorConfig::from_path(Some(&config));
    let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
    let mut marks = Marks {
        control,
        day: day_start_ms(now_secs()),
        ..Marks::default()
    };
    let (mut watcher, mut claudes) = (None, None);
    let ran = world.and_then(|world| {
        lay_world(&run, &world)?;
        watcher = Some(Watcher::start(&run));
        claudes = Some(ClaudeWatch::start(&run));
        let _ = crate::procs::jailed(
            &run,
            &run.w,
            &[],
            &[
                "/bin/sh",
                "-c",
                "/bin/sleep 2; :",
                lp::PROC_CONTROL,
                "auth",
                "login",
            ],
            crate::procs::JailOut::Capture,
        );
        let control = crate::recorder::control(&run);
        let mut d = Drive {
            run: &run,
            door: Door::new(&run).map_err(|e| die("the lab's door", e))?,
            world: &world,
            sid: String::new(),
            day: marks.day,
            grace,
        };
        let r = scenarios(&mut d, &mut marks);
        Ok((world, control, r))
    });
    let end = now_ms();
    for leg in [
        &mut marks.r1,
        &mut marks.r3,
        &mut marks.r4,
        &mut marks.r5a,
        &mut marks.r5b,
        &mut marks.r2,
    ] {
        if leg.from > 0 && leg.to == 0 {
            leg.to = end;
        }
    }
    let kept_text = ran
        .as_ref()
        .ok()
        .map(|(world, _, _)| Kept::read(&run, world, &marks.sid))
        .unwrap_or_default();
    let dirs = ran
        .as_ref()
        .ok()
        .map(|(w, _, _)| w.dirs())
        .unwrap_or_default();
    let witness_live = crate::procs::need_witness(&run).is_ok();
    let witness = ran
        .as_ref()
        .ok()
        .map(|(_, control, _)| crate::recorder::Witness::read(&run, control))
        .unwrap_or_default();
    let seen = watcher.map(Watcher::finish).unwrap_or_default();
    let launches = claudes.map(ClaudeWatch::finish).unwrap_or_default();
    let launch_text: String = launches
        .iter()
        .map(|l| format!("{}\n", launch_line(l)))
        .collect();
    let left_up = || {
        println!(
            "claude-rotation-proof: lab '{tag}' left up ({} down {tag})",
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
        kept_text.save(kept, &launch_text, &marks);
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
    let launched = launches_of(&launch_text);
    let ev = ev_of(&kept_text, &marks, &launched, dirs);
    let mut rows = judge_run(
        &ev,
        &kept_text,
        &marks,
        &launched,
        (&seen, &witness, witness_live),
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
    if marks.control {
        let control = judge_control(&ev, marks.start, &marks.r1, &rows);
        rows.insert(1, ("control", control));
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
#[path = "claude_rotation_proof_tests.rs"]
mod tests;
