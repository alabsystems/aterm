// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A person's click on a Claude Code tab's permission chip (`yolo`/`lame`): the one look the
//! window's click lane makes for the ask it holds ([`look`]), and what that look found ([`Look`]).
//!
//! The ask stands in the window's memory while it waits, never on disk; the restart it makes is
//! [`Restart::Permission`], through [`super::relaunch::restart_fenced`]
//! (`docs/DESIGN-yolo-binary-2026-11-04.md` §3 and its Appendix).

use super::relaunch::{
    self, CLICK_CLAIMED, CLICK_COORDINATED, CLICK_PRESSED, CLICK_TAKEN_BACK, IN_FLIGHT_OTHER,
    NO_LANE, NO_SHELL, Restarted, Snapshot,
};
use super::upgrade::{self, Gate};
use super::upgrade_drive::{
    Kernel, Live, Opts, Report, St, TabRead, connect_reads, dim_placeholder, load_record, now_s,
    press_facts, roster_aside, screen, status_line, sweep_lock,
};
use crate::supervise::policy::turn_end::{PermissionTo, Restart, RestartAsk};

/// One click's standing ask, as the window holds it: tab `tab`'s conversation `conversation`, to
/// be restarted onto the line [`PermissionTo`] names, asked at Unix second `at` (its ask word,
/// `permission@<at>`, names its record: [`RestartAsk::of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Click {
    /// The tab clicked (`s-<hex>`).
    pub tab: String,
    /// The conversation the tab's agent held at the click.
    pub conversation: String,
    /// The line asked: no permission flag, or bypass.
    pub to: PermissionTo,
    /// The click's Unix second.
    pub at: i64,
}

impl Click {
    fn restart(&self) -> Restart {
        Restart::Permission {
            to: self.to,
            conversation: self.conversation.clone(),
        }
    }
}

/// What one look at a click found: whether it still stands ([`Look::Waits`], [`Look::InFlight`],
/// [`Look::Moved`]) or ends, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Look {
    /// Nothing was signalled, and the click stands: the lane looks again at its next pace.
    Waits(Wait),
    /// The click's own restart is in flight (signalled, or its relaunch typed and not yet
    /// adopted, or its restore look open), the carry's last word with it: the click stands and the
    /// next look carries it on.
    InFlight(String),
    /// The click's restart landed: its agent runs the line asked. The click ends.
    Done,
    /// The click's restart failed (its `failed:` word). The click ends.
    Failed(String),
    /// The click can never be made here. It ends, the hover saying why.
    Refused(Refusal),
    /// The click ends with nothing made.
    Ended(End),
    /// The tab's agent holds another conversation, which a restart afresh of the click's own
    /// started (`relaunch::starts_afresh`): the click stands, now for that conversation.
    Moved(String),
}

/// Why a click waits ([`Look::Waits`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// A turn runs.
    Turn,
    /// A box is open (an approval, a question).
    Box,
    /// A draft stands in the composer, a slash press's text among them.
    Draft,
    /// A person holds the tab, or its status could not be read.
    Held,
    /// A usage limit is on the screen.
    Limit,
    /// A login is on the screen.
    Login,
    /// A press of the ⬆️ stands, and goes first.
    Pressed,
    /// Another restart of the conversation is in flight.
    Restart,
    /// The tab has no agent, and a relaunch is due that brings one back.
    Relaunch,
    /// Anything else the look or the restart waited on, by its word (`changed`, `no-socket`, …).
    Other(String),
}

/// Why a click is refused ([`Look::Refused`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The agent is no shell's job: the tab's own program, or a job without job control.
    NotAShellJob,
    /// The shell's dialect is one no relaunch line is written in.
    Shell,
    /// The tab's agent is one no restart lane reads.
    NoLane,
    /// The agent's arguments cannot be read.
    ArgvUnread,
    /// Another supervisor (not a window's own host) claims the tab.
    Claimed,
    /// The tab is a credit-claim worker's (its trusted folder is the claim folder).
    ClaimWorker,
    /// The conversation's record is under native coordination that no constructor here restarts
    /// (`ATERM-KI-1106-3`): the tab's agent must end first.
    Coordinated,
    /// Any other refusal of the restart, by its word (an unknown permission flag's spelling, a
    /// one-shot line, …).
    Other(String),
}

/// Why a click ends with nothing made ([`Look::Ended`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The tab's agent is no Claude Code, and no relaunch is due.
    NotClaude,
    /// The tab's agent holds another conversation that no restart of the click's own started.
    ConversationLeft,
    /// The ask no longer stands at the fence (its `stands` read false): taken back before the
    /// signal, or its one restart signalled already.
    TakenBack,
}

/// THE CLICK LANE'S ONE LOOK (`docs/DESIGN-yolo-binary-2026-11-04.md` §3): judges and, when it
/// may, makes the restart click `click` asks of the Claude Code in tab `click.tab`, on the control
/// socket and harness state `opts` names, and says what it found as a [`Look`].
///
/// Contract for the window's lane:
/// - The window holds the ask (in memory, one per tab and conversation) and calls this on its own
///   thread, paced as the ⬆️ press lane is; a call can block while its restart's agent exits and
///   its relaunch lands. Nothing is written or typed while the click waits: its restart claims
///   its record only just before the signal, and restores what stood when the signal is refused.
/// - `stands` says, with no I/O, whether the window still holds this ask and has not had its one
///   restart signalled yet: read before the hand is taken and again just before the signal, a
///   false one ends the look [`End::TakenBack`] with nothing signalled. After the signal a
///   take-back changes nothing.
/// - `relaunch_due` is the window's `relaunch::Relaunches::pending` for the tab: with no agent in
///   the tab and none due, the click ends ([`End::NotClaude`]).
/// - [`Look::Waits`], [`Look::InFlight`] and [`Look::Moved`] keep the ask standing (re-keyed to
///   the new conversation on [`Look::Moved`]); every other answer ends it.
/// - `opts.only_sid` is set to the tab, its grace to 0 and `hand_back` to false here, whatever the
///   caller passed: the click's lane carries its own restart to its end, closing a resumed one's
///   restore look by `relaunch::confirm`.
///
/// Order, fixed: the conversation's record first (one retired unissued refuses the click
/// [`Refusal::Coordinated`]; its own restart in flight is carried, its restore look closed;
/// another restart in flight keeps the click standing, carried by its own
/// lane, whatever the snapshot says); then the snapshot's words; then the tab's trusted folder (`credit_offers::is_claim_folder`); then the roster's
/// claim; then `upgrade::press_gate` at grace 0; then the restart. Like the ⬆️ press, it reads no
/// `[harness] relaunch` switch.
#[must_use]
pub fn look(opts: &Opts, click: &Click, relaunch_due: bool, stands: &dyn Fn() -> bool) -> Look {
    let opts = lane_opts(opts, click);
    look_with(&opts, click, relaunch_due, stands, &Live, || {
        relaunch::snapshot(&opts)
    })
}

/// The click lane's [`Opts`] for `click` ([`look`]'s contract).
pub(super) fn lane_opts(opts: &Opts, click: &Click) -> Opts {
    Opts {
        only_sid: Some(click.tab.clone()),
        human_grace_s: 0,
        hand_back: false,
        ..opts.clone()
    }
}

/// [`look`] with process facts from `k` and the tab's agent read by `snap` (the seam a test
/// scripts the agent through); `opts` is already the lane's ([`lane_opts`]).
pub(super) fn look_with(
    opts: &Opts,
    click: &Click,
    relaunch_due: bool,
    stands: &dyn Fn() -> bool,
    k: &dyn Kernel,
    snap: impl FnOnce() -> Result<Snapshot, &'static str>,
) -> Look {
    let why = click.restart();
    let ask = RestartAsk::of(&why, click.at);
    let prior = match load_record(opts, &click.conversation) {
        Ok(prior) => prior,
        Err(_) => return Look::Waits(Wait::Other("record-unreadable".to_string())),
    };
    // A record retired unissued (its native command positively not granted) is in flight for
    // good, and every restart over it waits (`native_restart::WAIT_RETIRED_UNISSUED`), the
    // click's own included: a record under native coordination that no constructor here
    // restarts, so the click is refused rather than left standing behind a restart nothing
    // carries (design Appendix, "Coordinated").
    if prior.as_ref().is_some_and(|st| st.native_retired_unissued) {
        return Look::Refused(Refusal::Coordinated);
    }
    if let Some(st) = prior.as_ref().filter(|st| own(&why, &ask, st, click))
        && let Some(found) = own_record(opts, click, st)
    {
        return found;
    }
    // Another restart of the conversation in flight is that restart's to carry: the click stands,
    // whatever the tab's agent reads as meanwhile, and carries none of it.
    if prior.as_ref().is_some_and(St::in_flight) {
        return Look::Waits(Wait::Restart);
    }
    let snap = match snap() {
        Ok(snap) => snap,
        Err(word) => return snapshot_word(word, relaunch_due),
    };
    if !matches!(snap.run, relaunch::AgentRun::Claude) {
        return Look::Ended(End::NotClaude);
    }
    if let Some(held) = snap.session.as_deref().filter(|s| *s != click.conversation) {
        return if prior
            .as_ref()
            .is_some_and(|st| started_afresh(st, click, &snap))
        {
            Look::Moved(held.to_string())
        } else {
            Look::Ended(End::ConversationLeft)
        };
    }
    if snap.shell == NO_SHELL {
        return Look::Refused(Refusal::NotAShellJob);
    }
    if let Some(stop) = tab_gate(opts, click, &snap) {
        return stop;
    }
    let Restarted { report, .. } = relaunch::restart_fenced(
        opts,
        &why,
        &ask,
        &super::accounts::Roster::default(),
        Ok(snap),
        None,
        k,
        stands,
        None,
    );
    let after = load_record(opts, &click.conversation).ok().flatten();
    match after.as_ref().filter(|st| own(&why, &ask, st, click)) {
        Some(st) if st.in_flight() || st.confirming() => Look::InFlight(report.step),
        Some(st) if matches!(st.phase, upgrade::Phase::Done) => Look::Done,
        Some(st) if matches!(st.phase, upgrade::Phase::Failed(_)) => Look::Failed(report.step),
        _ => restart_word(&report.step),
    }
}

/// Whether `st` is the record of `click`'s own restart: of its tab, by its cause and its ask
/// (`relaunch::its_own`).
fn own(why: &Restart, ask: &RestartAsk, st: &St, click: &Click) -> bool {
    st.tab == click.tab && relaunch::its_own(why, ask, st)
}

/// What the click's own record `st` answers before any snapshot: its restart carried while in
/// flight, its restore look closed by `relaunch::confirm`, its landing or its failure; `None` for
/// a record that no longer says anything of it.
fn own_record(opts: &Opts, click: &Click, st: &St) -> Option<Look> {
    if st.in_flight() {
        let carried = relaunch::carry_restart(opts, upgrade::Agent::Claude, Some(st.pid), false);
        return Some(
            match load_record(opts, &click.conversation).ok().flatten() {
                Some(after) if after.in_flight() || after.confirming() => {
                    Look::InFlight(carried.step)
                }
                Some(after) if matches!(after.phase, upgrade::Phase::Done) => Look::Done,
                Some(after) if matches!(after.phase, upgrade::Phase::Failed(_)) => {
                    Look::Failed(carried.step)
                }
                _ => Look::InFlight(carried.step),
            },
        );
    }
    if st.confirming() {
        let _held = match sweep_lock(opts) {
            Ok(lock) => lock,
            Err(why) => return Some(Look::Waits(Wait::Other(format!("busy:{why}")))),
        };
        let Some(mut st) = load_record(opts, &click.conversation)
            .ok()
            .flatten()
            .filter(St::confirming)
        else {
            return Some(Look::Done);
        };
        let r = Report {
            pid: st.resumed_pid,
            tab: st.tab.clone(),
            session: click.conversation.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.resumed_on, st.source),
            step: String::new(),
        };
        return Some(
            match relaunch::confirm(opts, &r, &mut st, &click.conversation, now_s()) {
                Some(closed) if closed.step.starts_with("failed:") => Look::Failed(closed.step),
                Some(_) => Look::Done,
                None => Look::InFlight("wait:restore".to_string()),
            },
        );
    }
    match &st.phase {
        upgrade::Phase::Done => Some(Look::Done),
        upgrade::Phase::Failed(why) => Some(Look::Failed(format!("failed:{why}"))),
        _ => None,
    }
}

/// The look's answer to the snapshot's word `word`, with no restart in flight.
fn snapshot_word(word: &str, relaunch_due: bool) -> Look {
    match word {
        "not-a-shell-job" => Look::Refused(Refusal::NotAShellJob),
        "shell-dialect" => Look::Refused(Refusal::Shell),
        "argv-unreadable" => Look::Refused(Refusal::ArgvUnread),
        w if w == NO_LANE => Look::Refused(Refusal::NoLane),
        "not-claude" if relaunch_due => Look::Waits(Wait::Relaunch),
        "not-claude" => Look::Ended(End::NotClaude),
        w => Look::Waits(Wait::Other(w.to_string())),
    }
}

/// How long after a restart afresh of the click's conversation landed its agent may have been
/// born and still be that restart's ([`started_afresh`]).
const AFRESH_SPAN_S: u64 = relaunch::STOPPED_OWN_S;

/// Whether `st`, the click's conversation's record, is a restart afresh
/// (`relaunch::starts_afresh`) that started the agent `snap` read: landed in the click's tab, on
/// the same shell, the agent another than the one it ended and born after that one was seen gone
/// and by the landing, within [`AFRESH_SPAN_S`] of it.
fn started_afresh(st: &St, click: &Click, snap: &Snapshot) -> bool {
    let born = super::upgrade_models::parse_lstart(&snap.start);
    matches!(st.phase, upgrade::Phase::Done)
        && relaunch::starts_afresh(&st.cause)
        && st.tab == click.tab
        && st.shell == snap.shell
        && st.pid != snap.pid
        && st.done_at != 0
        && born.is_some_and(|born| {
            born >= st.exited_at
                && born <= st.done_at
                && st.done_at.saturating_sub(born) <= AFRESH_SPAN_S
        })
}

/// The tab's own gates, in order: its trusted folder a credit-claim worker's, another
/// supervisor's claim, then the press's gate at grace 0 ([`upgrade::press_gate`]). `None` lets the
/// restart be asked.
fn tab_gate(opts: &Opts, click: &Click, snap: &Snapshot) -> Option<Look> {
    let tab = click.tab.as_str();
    let unread = |what: &str| Some(Look::Waits(Wait::Other(format!("{what}-unread"))));
    let Ok(mut c) = connect_reads(opts, tab) else {
        return Some(Look::Waits(Wait::Other("no-socket".to_string())));
    };
    let Ok(meta) = c.request_line(&format!("@{tab} meta")) else {
        return unread("meta");
    };
    if !crate::is_ok_reply(&meta) {
        return unread("meta");
    }
    if trusted_folder(&meta).is_some_and(|folder| super::credit_offers::is_claim_folder(&folder)) {
        return Some(Look::Refused(Refusal::ClaimWorker));
    }
    let Some(aside) = roster_aside(&mut c, &[]) else {
        return unread("roster");
    };
    if aside.claimed.contains(tab) {
        return Some(Look::Refused(Refusal::Claimed));
    }
    let Some(sf) =
        super::upgrade_drive::Records::of_snapshot(&opts.home, snap).session_file(snap.pid)
    else {
        return Some(Look::Waits(Wait::Other("no-session-record".to_string())));
    };
    let Some(scr) = screen(&mut c, tab) else {
        return unread("screen");
    };
    let status = status_line(&mut c, tab);
    let read = TabRead {
        scr: &scr,
        status: status.as_deref(),
        dim: dim_placeholder(&mut c, tab, &scr),
    };
    match upgrade::press_gate(&press_facts(&sf.status, &read)) {
        Gate::Go => None,
        Gate::Wait(word) => Some(Look::Waits(press_wait(word))),
    }
}

/// The folder a `meta` reply's `trusted_cwd=` names (`-`: none).
fn trusted_folder(meta: &str) -> Option<std::path::PathBuf> {
    meta.split_whitespace()
        .find_map(|w| w.strip_prefix("trusted_cwd="))
        .filter(|v| *v != "-" && !v.is_empty())
        .map(aterm_codec::pct::decode)
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// The click's wait for the press gate's word `word`.
fn press_wait(word: &str) -> Wait {
    match word {
        "held" => Wait::Held,
        "box" => Wait::Box,
        "draft" => Wait::Draft,
        "limited" => Wait::Limit,
        "login" => Wait::Login,
        "busy" => Wait::Turn,
        other => Wait::Other(other.to_string()),
    }
}

/// The look's answer to the restart's word `step`, where no record of the click's own says more.
fn restart_word(step: &str) -> Look {
    match step {
        CLICK_TAKEN_BACK => Look::Ended(End::TakenBack),
        CLICK_PRESSED => Look::Waits(Wait::Pressed),
        IN_FLIGHT_OTHER => Look::Waits(Wait::Restart),
        CLICK_CLAIMED => Look::Refused(Refusal::Claimed),
        CLICK_COORDINATED => Look::Refused(Refusal::Coordinated),
        relaunch::ALREADY_MADE => Look::Done,
        "wait:held" => Look::Waits(Wait::Held),
        "wait:shell-dialect" => Look::Refused(Refusal::Shell),
        "refused:not-a-shell-job" => Look::Refused(Refusal::NotAShellJob),
        s if s.starts_with("failed:") => Look::Failed(s.to_string()),
        s => match s.strip_prefix("refused:") {
            Some(what) => Look::Refused(Refusal::Other(what.to_string())),
            None => Look::Waits(Wait::Other(
                s.strip_prefix("wait:").unwrap_or(s).to_string(),
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no restart in flight the snapshot's words refuse or end a click, and a tab with no
    /// agent waits only while a relaunch is due.
    #[test]
    fn the_snapshots_words_refuse_or_end_a_click() {
        assert_eq!(
            snapshot_word("not-a-shell-job", false),
            Look::Refused(Refusal::NotAShellJob)
        );
        assert_eq!(
            snapshot_word("shell-dialect", false),
            Look::Refused(Refusal::Shell)
        );
        assert_eq!(
            snapshot_word(NO_LANE, false),
            Look::Refused(Refusal::NoLane)
        );
        assert_eq!(
            snapshot_word("argv-unreadable", false),
            Look::Refused(Refusal::ArgvUnread)
        );
        assert_eq!(
            snapshot_word("not-claude", false),
            Look::Ended(End::NotClaude)
        );
        assert_eq!(
            snapshot_word("not-claude", true),
            Look::Waits(Wait::Relaunch)
        );
        assert_eq!(
            snapshot_word("no-foreground", false),
            Look::Waits(Wait::Other("no-foreground".to_string()))
        );
    }

    /// A `meta` reply's trusted folder, pct-decoded; `-` and a relative path name none.
    #[test]
    fn a_trusted_folder_is_read_off_meta() {
        assert_eq!(
            trusted_folder("OK cwd=/a trusted_cwd=/s/credit%20claims"),
            Some(std::path::PathBuf::from("/s/credit claims"))
        );
        assert_eq!(trusted_folder("OK cwd=/a trusted_cwd=-"), None);
        assert_eq!(trusted_folder("OK cwd=/a trusted_cwd=rel"), None);
        assert_eq!(trusted_folder("OK cwd=/a"), None);
    }
}
