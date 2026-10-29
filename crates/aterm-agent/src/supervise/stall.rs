// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's hold on a worker that has stopped reading its input.

use super::super::journal::{ATTENTION_SKIPPED, ESCALATED_BEHIND, FROZEN};
use super::escalate::status_field;
use super::*;
use crate::harness::resume::{AfterRestart, RELAUNCH_WORDS, restart_command};

/// A worker the server says is not reading its input (`status input=stalled|stopped`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerStall {
    /// `stalled` (frozen) or `stopped` (a stopped job with input queued).
    pub(super) word: &'static str,
    /// `input_bytes=`: unread bytes, the kernel's queue and aterm's spill.
    pub(super) bytes: Option<u64>,
    /// `input_wait_ms=`: a lower bound on the oldest unread byte's wait.
    pub(super) wait_ms: Option<u64>,
    /// `fg_rss_mb=`: the foreground program's resident size.
    pub(super) rss_mb: Option<u64>,
}

/// What one `status` read said of the worker's input and session state.
pub(super) struct StallRead {
    word: String,
    stall: Option<WorkerStall>,
    program: Option<String>,
    /// `human_ms=`: how long ago a person gave the session input (`None`:
    /// never, or a server that does not say).
    human_ms: Option<u64>,
    /// `hand=`: whose hand is on the session (`-` or absent: nobody's).
    hand: Option<String>,
}

/// Attention reason when an ask's badge is withheld during a worker stall.
pub(super) const WITHHELD: &str = "the same status says the worker is not reading its input";

/// Whether a `status` reply carries a measured `input=` word.
pub(super) fn publishes_input(status_stdout: &str) -> bool {
    status_field(status_stdout, "input").is_some()
}

/// Parses `WorkerStall` from a `status` reply for `input=stalled|stopped`.
pub(super) fn worker_stall(status_stdout: &str) -> Option<WorkerStall> {
    let word = match status_field(status_stdout, "input")? {
        "stalled" => "stalled",
        "stopped" => "stopped",
        _ => return None,
    };
    let num = |field: &str| status_field(status_stdout, field).and_then(|v| v.parse().ok());
    Some(WorkerStall {
        word,
        bytes: num("input_bytes"),
        wait_ms: num("input_wait_ms"),
        rss_mb: num("fg_rss_mb"),
    })
}

/// A wait as the band says one: `41s`, `2m03s`, `2h41m`, `1d 3h`.
fn wait_text(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// A resident size as the server's attention says one: `rss 312 MB` under a
/// GiB, else tenths of a GiB (39731 MiB reads `rss 38.8 GB`).
fn rss_text(mb: u64) -> String {
    if mb < 1024 {
        return format!("rss {mb} MB");
    }
    let tenths = (u128::from(mb) * 10 + 512) / 1024;
    format!("rss {}.{} GB", tenths / 10, tenths % 10)
}

/// Whether `restart unavailable` is a transient hand refusal (`halted`, `driver-lease`, `driver-turn`).
pub(super) fn refusal_lifts(r: &CtlReply) -> bool {
    ["halted", "driver-lease", "driver-turn"]
        .iter()
        .any(|why| r.is_err(&format!("restart unavailable: {why}")))
}

/// Formats a `CtlReply` as a single journal line.
pub(super) fn reply_line(r: &CtlReply) -> String {
    if r.ok() {
        return r.stdout.lines().next().unwrap_or("OK").trim().to_string();
    }
    let e = r.err_text();
    if e.is_empty() {
        r.stdout.trim().to_string()
    } else {
        e.to_string()
    }
}

/// What the frozen warning says of a stall after its word: how long
/// the input has waited, then what is queued and the resident size —
/// ` for 2m41s (1 B queued, rss 38.8 GB)`, each part only where the reply
/// measured it.
fn stall_facts(stall: &WorkerStall) -> String {
    let waited = stall
        .wait_ms
        .map_or_else(String::new, |ms| format!(" for {}", wait_text(ms)));
    let facts: Vec<String> = stall
        .bytes
        .map(|n| format!("{n} B queued"))
        .into_iter()
        .chain(stall.rss_mb.map(rss_text))
        .collect();
    if facts.is_empty() {
        waited
    } else {
        format!("{waited} ({})", facts.join(", "))
    }
}

/// Builds the two-line frozen-worker warning text for the host.
pub(super) fn frozen_warning_text(
    stall: &WorkerStall,
    program: &str,
    sid: Option<&str>,
    after: &AfterRestart,
    pid: Option<u32>,
) -> String {
    let facts = stall_facts(stall);
    let act = || restart_command(Some(sid.unwrap_or("<sid>")), pid);
    match after {
        AfterRestart::Relaunch => {
            return format!(
                "{program} frozen: not reading input{facts}\naterm leaves it running \u{2014} \
                 `{}` ends it, and {RELAUNCH_WORDS}",
                act()
            );
        }
        AfterRestart::Act => {
            return format!(
                "{program} frozen: not reading input{facts}\naterm leaves it running \u{2014} \
                 `{}` restarts it",
                act()
            );
        }
        AfterRestart::Unknown | AfterRestart::Command(_) => {}
    }
    let sid = sid.map_or("<sid>", |s| s.trim_start_matches('@'));
    let end = format!(
        "aterm leaves it running \u{2014} `aterm ctl @{sid} signal term` ends it (`signal kill` \
         if it survives)"
    );
    let then = match after {
        AfterRestart::Unknown => String::new(),
        AfterRestart::Command(line) => std::iter::once(line.clone())
            .chain(crate::harness::resume::shorter(line))
            .map(|l| format!(", then `{l}` resumes it"))
            .find(|then| end.chars().count() + then.chars().count() <= NOTE_LINE_CHARS)
            .unwrap_or_default(),
        AfterRestart::Relaunch | AfterRestart::Act => String::new(),
    };
    format!("{program} frozen: not reading input{facts}\n{end}{then}")
}

/// The characters the window keeps of each further line of a supervisor's note
/// (`aterm_messages::DETAIL_LINE_CAP`, which cuts past it).
const NOTE_LINE_CHARS: usize = 240;

impl<C: Ctl> Session<'_, C> {
    /// Records input capability and stall suspicion from a `status` reply.
    pub(super) fn note_status(&mut self, args: &[&str], r: &CtlReply) {
        if args.first() == Some(&"status") && r.ok() {
            self.caps.input = Some(publishes_input(&r.stdout));
            self.caps.log = Some(super::log_wake::LogHead::of_status(&r.stdout).is_some());
            if self.stalled.is_none() && worker_stall(&r.stdout).is_some() {
                self.stall_suspect = true;
            }
        }
    }

    /// Whether a suspected stall is in hand while unattended and not yet held.
    pub(super) fn stall_in_hand(&self, review: &dyn Review) -> bool {
        review.unattended() && self.stall_suspect && self.stalled.is_none()
    }

    /// Holds a suspected stall before entering `wait_for_next`.
    pub(super) fn stall_before_wait(
        &mut self,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if self.stall_in_hand(review) {
            self.stall_step(seq, review)?;
        }
        Ok(())
    }

    /// Withholds an ask badge when a worker stall is in hand.
    pub(super) fn withhold_badge(
        &mut self,
        seq: u64,
        kind: super::escalate::Escalation,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if kind != super::escalate::Escalation::Ask
            || !review.unattended()
            || self.stalled.is_some()
        {
            return Ok(false);
        }
        if !self.stall_suspect && self.caps.input != Some(false) {
            let r = self.call(&["status"])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
            }
        }
        if !self.stall_in_hand(review) {
            return Ok(false);
        }
        review.note(&format!(
            "ESCALATED seq={seq} {ATTENTION_SKIPPED}{WITHHELD}"
        ));
        Ok(true)
    }

    /// What follows the stall's restart, for the warning
    /// ([`frozen_warning_text`]): the explicit restart where
    /// the host would carry the act ([`super::IdleHost::restart_offered`],
    /// the one predicate the server's own words choose by) — with the host's
    /// relaunch on its conversation only where that is promised — else the
    /// line resuming the worker's own conversation
    /// ([`super::IdleHost::resume_command`]), else nothing (a loop with no
    /// host reads no process).
    fn after_restart(&self) -> AfterRestart {
        let Some(host) = &self.stall_host else {
            return AfterRestart::Unknown;
        };
        match host.restart_offered() {
            Some(true) => AfterRestart::Relaunch,
            Some(false) => AfterRestart::Act,
            None => host
                .resume_command()
                .map_or(AfterRestart::Unknown, AfterRestart::Command),
        }
    }

    /// Reads `status` once to inspect the worker's input state.
    pub(super) fn stall_probe(&mut self) -> Result<Option<StallRead>, Fail> {
        let r = self.call(&["status"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
        }
        if !r.ok() || !publishes_input(&r.stdout) {
            return Ok(None);
        }
        let field = |f: &str| status_field(&r.stdout, f).map(str::to_string);
        Ok(Some(StallRead {
            word: field("input").unwrap_or_default(),
            stall: worker_stall(&r.stdout),
            program: field("program"),
            human_ms: field("human_ms").and_then(|v| v.parse().ok()),
            hand: field("hand"),
        }))
    }

    /// Enters the stall hold, unsetting our point badge and journaling `FROZEN`.
    pub(super) fn hold_for_stall(
        &mut self,
        read: StallRead,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(stall) = read.stall.clone() else {
            return Ok(());
        };
        let read_rest = read;
        self.stall_suspect = false;
        self.box_ask = None;
        self.adopted_box = false;
        self.turn_end_due = None;
        self.stall_termed = false;
        let behind = self.claim.watching_behind().map(str::to_string);
        let attention = if let Some(holder) = behind {
            format!("{ATTENTION_SKIPPED}{ESCALATED_BEHIND} ({holder}) answers this session")
        } else {
            let attention = self.unset_if_ours(|t| !t.starts_with(ATTENTION_PREFIX))?;
            if self.attention_text().is_none() {
                // The turn-end badge's desired state was cleared too.
                self.turn_end_badge = false;
            }
            format!("attention={attention}")
        };
        let dash = || "-".to_string();
        review.note(&format!(
            "{FROZEN} seq={seq} input={} wait_ms={} bytes={} rss_mb={} {attention}",
            stall.word,
            stall.wait_ms.map_or_else(dash, |v| v.to_string()),
            stall.bytes.map_or_else(dash, |v| v.to_string()),
            stall.rss_mb.map_or_else(dash, |v| v.to_string()),
        ));
        self.stalled = Some(stall);
        if let Some(host) = &self.stall_host {
            host.stalled(true);
        }
        self.stall_remedy(&read_rest, seq, review)
    }

    /// Exits the stall hold, journaling `THAWED` and clearing the handed point.
    pub(super) fn thaw(
        &mut self,
        seq: u64,
        word: &str,
        state: &mut Looking,
        review: &mut dyn Review,
    ) {
        self.stall_termed = false;
        if self.stalled.take().is_some() {
            review.note(&format!("THAWED seq={seq} input={}", or_dash(word)));
            state.handed = None;
            if let Some(host) = &self.stall_host {
                host.stalled(false);
            }
        }
    }

    /// Applies the stall remedy (warning or `restart if-stalled`) once past `stall_term_after_s`.
    pub(super) fn stall_remedy(
        &mut self,
        read: &StallRead,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(stall) = self.stalled.clone() else {
            return Ok(());
        };
        let relaunched = self
            .stall_host
            .as_ref()
            .is_some_and(|h| h.relaunches_after_stall());
        let grace_ms = u64::from(self.stall_grace_s).saturating_mul(1000);
        let hands_off = read.hand.as_deref().is_none_or(|h| h == "-")
            && read.human_ms.is_none_or(|ms| ms >= grace_ms);
        if stall.word != "stalled"
            || !review.unattended()
            || !relaunched
            || !hands_off
            || self.claim.remedies_withheld()
        {
            return Ok(());
        }
        let bound_ms = u64::from(self.stall_term_after_s).saturating_mul(1000);
        if self.stall_termed || bound_ms == 0 || !stall.wait_ms.is_some_and(|ms| ms >= bound_ms) {
            return Ok(());
        }
        if !self.stall_term {
            self.stall_termed = true;
            let after = self.after_restart();
            return self.warn_stall(
                read,
                &stall,
                &after,
                seq,
                review,
                "(`[harness] stall_term = true` ends it)",
            );
        }
        let leader = self.stall_host.as_ref().and_then(|h| h.agent_pid());
        let pid = leader.map(|p| format!("pid={p}"));
        let quiet = format!("quiet={}", self.stall_grace_s);
        let mut args = vec!["restart"];
        if let Some(pid) = pid.as_deref() {
            args.push(pid);
        }
        args.extend(["if-stalled", quiet.as_str()]);
        let r = self.call(&args)?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("restart failed: {}", r.stderr.trim())));
        }
        if crate::supervise::report::without_verb(&r) {
            return self.stall_signal_term(&stall, seq, review);
        }
        let reply = reply_line(&r);
        if r.is_err("restart unavailable") && !refusal_lifts(&r) {
            self.stall_termed = true;
            let after = self.stall_host.as_ref().map_or(AfterRestart::Unknown, |h| {
                h.resume_command()
                    .map_or(AfterRestart::Unknown, AfterRestart::Command)
            });
            return self.warn_stall(read, &stall, &after, seq, review, &format!("({reply})"));
        }
        if r.ok() {
            self.stall_termed = true;
        }
        let dash = || "-".to_string();
        review
            .say(&format!(
                "{} seq={seq} input={} wait_ms={} rss_mb={} reply={reply}",
                if r.ok() {
                    "RESTARTED"
                } else {
                    "RESTART-REFUSED"
                },
                stall.word,
                stall.wait_ms.map_or_else(dash, |v| v.to_string()),
                stall.rss_mb.map_or_else(dash, |v| v.to_string()),
            ))
            .map_err(Fail::Hard)?;
        Ok(())
    }

    /// The warning in place of the remedy, once an episode: said `STALL-WARNED … ended=no <why>`,
    /// and [`frozen_warning_text`] given to the host.
    fn warn_stall(
        &mut self,
        read: &StallRead,
        stall: &WorkerStall,
        after: &AfterRestart,
        seq: u64,
        review: &mut dyn Review,
        why: &str,
    ) -> Result<(), Fail> {
        let rows = self.last.as_ref().map_or(&[][..], |s| s.rows.as_slice());
        let reader = aterm_phase::identify(read.program.as_deref(), rows).program();
        let program = read.program.as_deref().unwrap_or(reader.name());
        let leader = self.stall_host.as_ref().and_then(|h| h.agent_pid());
        let text = frozen_warning_text(stall, program, self.setup.sid.as_deref(), after, leader);
        if let Some(host) = &self.stall_host {
            host.inform(&text);
        }
        let dash = || "-".to_string();
        review
            .say(&format!(
                "STALL-WARNED seq={seq} input={} wait_ms={} rss_mb={} ended=no {why}",
                stall.word,
                stall.wait_ms.map_or_else(dash, |v| v.to_string()),
                stall.rss_mb.map_or_else(dash, |v| v.to_string()),
            ))
            .map_err(Fail::Hard)
    }

    /// The remedy for a server without the `restart` verb: the bare `signal term`, counted once
    /// sent.
    fn stall_signal_term(
        &mut self,
        stall: &WorkerStall,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let sig = "term";
        let r = self.call(&["signal", sig])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("signal failed: {}", r.stderr.trim())));
        }
        let reply = reply_line(&r);
        if r.ok() {
            self.stall_termed = true;
        }
        let dash = || "-".to_string();
        review
            .say(&format!(
                "SIGNALLED seq={seq} signal={sig} input={} wait_ms={} rss_mb={} reply={reply}",
                stall.word,
                stall.wait_ms.map_or_else(dash, |v| v.to_string()),
                stall.rss_mb.map_or_else(dash, |v| v.to_string()),
            ))
            .map_err(Fail::Hard)?;
        Ok(())
    }

    /// Probes input stall state after an unlatched wait step.
    pub(super) fn stall_step(&mut self, seq: u64, review: &mut dyn Review) -> Result<bool, Fail> {
        if !review.unattended()
            || (self.caps.input == Some(false) && self.stalled.is_none() && !self.stall_suspect)
        {
            return Ok(false);
        }
        let read = self.stall_probe()?;
        self.stall_suspect = false;
        let Some(read) = read else {
            return Ok(self.stalled.is_some());
        };
        if read.stall.is_none() {
            return Ok(self.stalled.is_some());
        }
        if self.stalled.is_some() {
            self.stalled.clone_from(&read.stall);
            self.stall_remedy(&read, seq, review)?;
        } else {
            self.hold_for_stall(read, seq, review)?;
        }
        Ok(false)
    }

    /// Checks held or suspected stall state at the start of a look.
    pub(super) fn stall_look(
        &mut self,
        seq: u64,
        state: &mut Looking,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if self.stalled.is_none() && !std::mem::take(&mut self.stall_suspect) {
            return Ok(false);
        }
        let read = self.stall_probe()?;
        match read {
            Some(read) if read.stall.is_some() => {
                if self.stalled.is_some() {
                    self.stalled.clone_from(&read.stall);
                    self.stall_remedy(&read, seq, review)?;
                } else {
                    self.hold_for_stall(read, seq, review)?;
                }
                Ok(true)
            }
            other => {
                let word = other.map_or_else(String::new, |r| r.word);
                self.thaw(seq, &word, state, review);
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stalled reading, as `status` spells it.
    const STALLED: &str = "OK schema=1 sid=s-1 hold=0 fabric=connected program=claude \
                           agent=wall:unresponsive integration=- input=stalled input_bytes=1 \
                           input_wait_ms=161000 fg_rss_mb=39731 supervisor=aterm-harness@1 \
                           seq=9 hash=0\n";

    #[test]
    fn a_stalled_or_stopped_input_is_a_stall_and_every_other_word_is_not() {
        assert_eq!(
            worker_stall(STALLED),
            Some(WorkerStall {
                word: "stalled",
                bytes: Some(1),
                wait_ms: Some(161_000),
                rss_mb: Some(39_731),
            })
        );
        let stopped = "OK input=stopped input_bytes=3 input_wait_ms=41000 fg_rss_mb=-\n";
        let s = worker_stall(stopped).expect("stopped is a stall");
        assert_eq!((s.word, s.bytes, s.rss_mb), ("stopped", Some(3), None));
        for line in [
            "OK input=clear input_bytes=0 input_wait_ms=0 fg_rss_mb=-\n",
            "OK input=pending input_bytes=1 input_wait_ms=1500 fg_rss_mb=-\n",
            "OK input=typeahead input_bytes=4 input_wait_ms=30000 fg_rss_mb=-\n",
            "OK input=- input_bytes=- input_wait_ms=- fg_rss_mb=-\n",
            "OK schema=1 hold=0 fabric=absent program=claude\n",
            "",
        ] {
            assert_eq!(worker_stall(line), None, "{line}");
        }
        assert!(publishes_input(STALLED));
        assert!(publishes_input("OK input=clear\n"));
        assert!(!publishes_input("OK input=- input_bytes=-\n"));
        assert!(!publishes_input("OK schema=1 hold=0\n"));
    }

    /// What a stall's warning says after its word: the wait, the backlog
    /// and the size, each only where the server measured it.
    #[test]
    fn the_stall_facts_name_the_wait_the_backlog_and_the_size() {
        let s = worker_stall(STALLED).expect("stall");
        assert_eq!(stall_facts(&s), " for 2m41s (1 B queued, rss 38.8 GB)");
        let small = WorkerStall {
            rss_mb: Some(12),
            wait_ms: None,
            ..s.clone()
        };
        assert_eq!(stall_facts(&small), " (1 B queued, rss 12 MB)");
        let bare = WorkerStall {
            bytes: None,
            rss_mb: None,
            wait_ms: Some(41_000),
            ..s
        };
        assert_eq!(stall_facts(&bare), " for 41s");
        assert_eq!(rss_text(u64::MAX), "rss 18014398509481984.0 GB");
    }
}
