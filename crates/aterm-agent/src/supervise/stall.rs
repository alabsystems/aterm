// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's hold on a worker that has stopped reading its input.
//!
//! A frozen worker's screen keeps showing what it last drew, and any key sent
//! is queued behind unread input, so waiting for a box to go would wait
//! forever. The server dates every input byte and `status` reports the oldest
//! unread one's wait (`input=clear|pending|typeahead|stalled|stopped
//! input_bytes=<n> input_wait_ms=<ms> fg_rss_mb=<n>`); past its threshold it
//! publishes `agent=wall:unresponsive` and an attention entry of its own. A
//! socket key behind input that has waited a second is still written, its `OK`
//! carrying `warn=input-unread`; only one behind a published stall (or a
//! stopped job) is refused `ERR busy input-unread`: a slow read is a
//! warning, not a block.
//!
//! On `input=stalled|stopped` ([`Session::hold_for_stall`]) the loop unsets its
//! own badge ([`Session::unset_if_ours`]), forgets the escalated box and the
//! turn-end decision, presses nothing, mails the manager once per episode
//! ([`frozen_mail_text`]) and journals `FROZEN …`. Any other word
//! ([`Session::thaw`]) journals `THAWED …` and the point is decided afresh.
//!
//! The remedy ([`Session::stall_remedy`]): once a stall of `input=stalled` has
//! stood `[harness] stall_term_after_s` (`0` never), with no person's hand
//! within `human_grace_s`, no other hand, and a host that relaunches the agent
//! ([`IdleHost::relaunches_after_stall`]), the loop warns once per episode
//! (`STALL-WARNED …`, [`frozen_warning_text`]), or with `stall_term = true`
//! sends the explicit restart, `restart pid=<leader> if-stalled
//! quiet=<grace>` (design of 2026-10-08): the server stamps the agent before
//! it signals it, so its exit is the restart's whoever reads it, and ends a
//! program that lives through its `SIGTERM`. The server's own fences decide
//! (`ERR not-stalled`, `busy`, `halted`: nothing, the next read decides;
//! `ERR restart unavailable`: counted, the warning said instead); an older
//! server that does not know the verb (`ERR unknown verb`, or `ERR denied`
//! for a verb aimed at a session) gets the bare `signal term`.
//!
//! [`Session::stall_step`] runs after every unlatched wait step
//! ([`Session::wait_for_next`]); [`Session::stall_look`] runs while a stall is
//! held or suspected ([`Session::stall_suspect`], set by a refused or warned
//! key). Every `status` the loop reads is checked ([`Session::note_status`]);
//! while a stall is in hand ([`Session::stall_in_hand`]) nothing is pressed,
//! typed, badged or mailed ([`Session::withhold_badge`], [`WITHHELD`]).
//! A host that does not measure input ([`Caps::input`]) costs at most one extra
//! `status` per connection.

use super::super::journal::{ATTENTION_SKIPPED, ESCALATED_BEHIND, FROZEN};
use super::escalate::status_field;
use super::*;
use crate::harness::resume::{AfterRestart, RELAUNCH_WORDS, restart_command};

/// A worker the server says is not reading its input: `status
/// input=stalled|stopped` and the numbers beside it (`-`, or absent, is
/// `None`).
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

/// What one `status` read said of the worker's input: its `input=` word,
/// the stall when that word is one, and the fields the hold and its remedy
/// need from the same read (`fabric=`, `program=`, `human_ms=`, `hand=`).
pub(super) struct StallRead {
    word: String,
    stall: Option<WorkerStall>,
    fabric: String,
    program: Option<String>,
    /// `human_ms=`: how long ago a person gave the session input (`None`:
    /// never, or a server that does not say).
    human_ms: Option<u64>,
    /// `hand=`: whose hand is on the session (`-` or absent: nobody's).
    hand: Option<String>,
}

/// Why an ask is withheld while a stall is in hand: the mail word of an
/// escalation whose own `status` read reported the stall
/// ([`Session::escalate`], `mail=skipped: <this>`) — an ask to answer a point
/// no answer can reach is not mailed; the hold's own mail tells the manager
/// what is wrong ([`frozen_mail_text`]) — and the attention word of an ask
/// whose badge a stall in hand kept down ([`Session::withhold_badge`],
/// [`ATTENTION_SKIPPED`] then this: no badge, so no person's wait).
pub(super) const WITHHELD: &str = "the same status says the worker is not reading its input";

/// Whether a `status` reply carries a measured `input=` word. An absent field
/// (an older aterm) and `input=-` (not macOS, not a tty) are both no.
pub(super) fn publishes_input(status_stdout: &str) -> bool {
    status_field(status_stdout, "input").is_some()
}

/// The stall a `status` reply reports: `Some` for `input=stalled` and
/// `input=stopped`, `None` for every other word and for a reply with none.
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
    let tenths = (mb * 10 + 512) / 1024;
    format!("rss {}.{} GB", tenths / 10, tenths % 10)
}

/// A reply's one line for the journal: an `OK`'s first line, else its error
/// (the server's `ERR …`, without the client's prefix).
fn reply_line(r: &CtlReply) -> String {
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

/// What the frozen mail and warning say of a stall after its word: how long
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

/// The one mail an episode posts to the manager:
///
/// ```text
/// claude frozen: not reading input for 2m41s (1 B queued, rss 38.8 GB); pressing nothing until it reads again — restart it: signal term (signal kill if it survives), then claude --resume <its conversation>
/// claude frozen: … — restart it: aterm ctl @s-1 restart pid=4242 if-stalled; aterm relaunches it on its conversation
/// codex frozen: … — restart it: aterm ctl @s-1 restart pid=4242 if-stalled
/// claude stopped with input queued for 41s (1 B queued); pressing nothing until it reads again — resume it: signal cont
/// ```
///
/// `program` names the worker (`status program=`, else `claude`); `after` is
/// what follows the restart ([`crate::harness::resume::AfterRestart`]): the
/// worker's own resume line, the host's relaunch on its conversation, or
/// nothing. Where the host carries the act (`Relaunch`, `Act`), the restart
/// named is the explicit one ([`crate::harness::resume::restart_command`]
/// for tab `sid`'s leader `pid`), which ends a survivor itself — and only a
/// `Relaunch` promises the conversation; elsewhere `signal term`, with
/// `signal kill` the fallback for a program that survives it. Never `claude
/// --continue`, which may resume a sibling tab's conversation.
pub(super) fn frozen_mail_text(
    stall: &WorkerStall,
    program: &str,
    after: &AfterRestart,
    sid: Option<&str>,
    pid: Option<u32>,
) -> String {
    let facts = stall_facts(stall);
    if stall.word == "stopped" {
        return format!(
            "{program} stopped with input queued{facts}; pressing nothing until it reads \
             again \u{2014} resume it: signal cont"
        );
    }
    let remedy = match after {
        AfterRestart::Unknown => "signal term (signal kill if it survives)".to_string(),
        AfterRestart::Command(line) => {
            format!("signal term (signal kill if it survives), then {line}")
        }
        AfterRestart::Relaunch => {
            format!("{}; {RELAUNCH_WORDS}", restart_command(sid, pid))
        }
        AfterRestart::Act => restart_command(sid, pid),
    };
    format!(
        "{program} frozen: not reading input{facts}; pressing nothing until it reads \
         again \u{2014} restart it: {remedy}"
    )
}

/// The warning the remedy's bound takes by default, for the person where they
/// look ([`super::IdleHost::inform`]): the state on its first line (the
/// record's title), what to do on the second:
///
/// ```text
/// claude frozen: not reading input for 11m00s (1 B queued, rss 38.8 GB)
/// aterm leaves it running — `aterm ctl @s-1 restart pid=4242 if-stalled` ends it, and aterm relaunches it on its conversation
/// ```
///
/// `after` is decided as the mail's is ([`Session::after_restart`]): where
/// the host carries the act, the explicit restart of the leader `pid`
/// ([`crate::harness::resume::restart_command`]), with the host's relaunch
/// ([`RELAUNCH_WORDS`]) only where that is promised; else `signal term` and
/// the resume line, else `signal term` alone. `sid` is the session's own id
/// where known.
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
    // The loop's selector (`@s-1`) or a bare id: either names it once.
    let sid = sid.map_or("<sid>", |s| s.trim_start_matches('@'));
    let end = format!(
        "aterm leaves it running \u{2014} `aterm ctl @{sid} signal term` ends it (`signal kill` \
         if it survives)"
    );
    let then = match after {
        AfterRestart::Unknown => String::new(),
        // Whole or not at all, as every remedy names it
        // ([`crate::harness::resume::never_cut`]): the line, else its
        // shorter forms (its permission mode alone, then bare), else none —
        // a cut id names no conversation, or another's.
        AfterRestart::Command(line) => std::iter::once(line.clone())
            .chain(crate::harness::resume::shorter(line))
            .map(|l| format!(", then `{l}` resumes it"))
            .find(|then| end.chars().count() + then.chars().count() <= NOTE_LINE_CHARS)
            .unwrap_or_default(),
        AfterRestart::Relaunch | AfterRestart::Act => String::new(),
    };
    format!("{program} frozen: not reading input{facts}\n{end}{then}")
}

/// The characters the window keeps of each further line of a supervisor's
/// note ([`super::IdleHost::inform`]; `aterm_messages::DETAIL_LINE_CAP`,
/// which cuts past it).
const NOTE_LINE_CHARS: usize = 240;

impl<C: Ctl> Session<'_, C> {
    /// Learn from any `status` reply the loop reads — for a box, a hold, the
    /// fabric — whether the host measures the worker's input
    /// ([`publishes_input`]), so the wait probes only a host that does; and
    /// a stall it reports that the loop does not hold yet is suspected
    /// ([`Self::stall_suspect`]), so nothing is pressed, typed or escalated
    /// on the strength of the screen it came with ([`Self::stall_in_hand`]).
    pub(super) fn note_status(&mut self, args: &[&str], r: &CtlReply) {
        if args.first() == Some(&"status") && r.ok() {
            self.caps.input = Some(publishes_input(&r.stdout));
            if self.stalled.is_none() && worker_stall(&r.stdout).is_some() {
                self.stall_suspect = true;
            }
        }
    }

    /// Whether a `status` read since the look began reported a stall this
    /// watch does not hold yet ([`Self::note_status`]): the point on the
    /// screen is not pressed, typed at, badged or asked about — no key
    /// reaches a program that is not reading, and no answer does either.
    pub(super) fn stall_in_hand(&self, review: &dyn Review) -> bool {
        review.unattended() && self.stall_suspect && self.stalled.is_none()
    }

    /// Before the first step of [`Self::wait_for_next`]: a stall a `status`
    /// read since the look reported — the escalation's own read of the
    /// fabric, which comes after its badge ([`Self::escalate`]), or, with no
    /// manager, the read that kept the badge down ([`Self::withhold_badge`])
    /// — is held now, a badge withdrawn before the wait, never a step into
    /// it.
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

    /// Before an ask's badge goes up ([`Self::escalate`]): `true` when a stall
    /// is in hand ([`Self::stall_in_hand`]), so no badge is raised and nothing
    /// is mailed; journaled with [`WITHHELD`]. With no manager, reads
    /// `status` here for this alone, unless the host measures nothing
    /// ([`Caps::input`]) or a stall is already suspected.
    pub(super) fn withhold_badge(
        &mut self,
        seq: u64,
        kind: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if kind != "ask" || !review.unattended() || self.stalled.is_some() {
            return Ok(false);
        }
        if self.manager.is_none() && !self.stall_suspect && self.caps.input != Some(false) {
            let r = self.call(&["status"])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
            }
        }
        if !self.stall_in_hand(review) {
            return Ok(false);
        }
        let mail = if self.manager.is_none() {
            "skipped: no --inbox and no $ATERM_PARENT_SESSION_ID".to_string()
        } else {
            format!("skipped: {WITHHELD}")
        };
        review.note(&format!(
            "ESCALATED seq={seq} {ATTENTION_SKIPPED}{WITHHELD} mail={mail}"
        ));
        Ok(true)
    }

    /// What follows the stall's restart, for the mail ([`frozen_mail_text`])
    /// and the warning ([`frozen_warning_text`]): the explicit restart where
    /// the host would carry the act ([`super::IdleHost::restart_offered`],
    /// the one predicate the server's own words choose by) — with the host's
    /// relaunch on its conversation only where that is promised — else the
    /// line resuming the worker's own conversation
    /// ([`super::IdleHost::resume_command`]), else nothing (`drive watch`
    /// reads no process).
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

    /// One `status` read of the worker's input; `None` when the host does not
    /// measure it (no reply it could give, no field, `input=-`).
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
            fabric: field("fabric").unwrap_or_else(|| "unknown".to_string()),
            program: field("program"),
            human_ms: field("human_ms").and_then(|v| v.parse().ok()),
            hand: field("hand"),
        }))
    }

    /// A stall seen: hold. The loop's own point badge is unset (the server's
    /// `aterm` entry says "frozen" over it, and would leave ours standing as
    /// "answer this box" the moment it cleared), the escalated box and the
    /// policy's due decision are forgotten, one `kind=ask` goes to the
    /// manager while the fabric is connected, and the episode is journaled
    /// `FROZEN …` ([`FROZEN`]). Nothing is pressed or typed here or while it
    /// stands. The badge's end is that line's: its `attention=<reply>` is the
    /// unset's, read back by the harness's stats as the end of the person's
    /// wait the badge opened (no `CLEARED` line follows); behind another
    /// supervisor it is [`ATTENTION_SKIPPED`]'s, nothing touched.
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
        let (attention, mail) = if let Some(holder) = behind {
            let why = format!("{ESCALATED_BEHIND} ({holder}) answers this session");
            (
                format!("{ATTENTION_SKIPPED}{why}"),
                format!("skipped: {why}"),
            )
        } else {
            let attention = self.unset_if_ours(|t| !t.starts_with(ATTENTION_PREFIX))?;
            if self.attention_ours.is_none() {
                // The turn-end policy's badge, if that was ours, went too.
                self.turn_end_badge = false;
            }
            let mail = match self.manager.clone() {
                None => "skipped: no --inbox and no $ATERM_PARENT_SESSION_ID".to_string(),
                Some(to) if read_rest.fabric == "connected" => {
                    let rows = self.last.as_ref().map_or(&[][..], |s| s.rows.as_slice());
                    let reader =
                        aterm_phase::identify(read_rest.program.as_deref(), rows).program();
                    let program = read_rest.program.as_deref().unwrap_or(reader.name());
                    let leader = self.stall_host.as_ref().and_then(|h| h.agent_pid());
                    let text = frozen_mail_text(
                        &stall,
                        program,
                        &self.after_restart(),
                        self.sid.as_deref(),
                        leader,
                    );
                    let to = format!("to={to}");
                    let r = self.call(&["post", &to, "kind=ask", "--wait=0", &text])?;
                    post_word(&r)
                }
                Some(_) => format!("skipped: no fabric (fabric={})", read_rest.fabric),
            };
            (format!("attention={attention}"), mail)
        };
        let dash = || "-".to_string();
        review.note(&format!(
            "{FROZEN} seq={seq} input={} wait_ms={} bytes={} rss_mb={} {attention} mail={mail}",
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

    /// The stall is over (`status` reads another word, or no longer measures
    /// the input): journaled `THAWED seq=<n> input=<w>`, and the point handed
    /// over before it is forgotten, so the look decides it again.
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

    /// The stall's remedy (module header): once the held stall has stood
    /// `[harness] stall_term_after_s` (`0` never), a warning by default
    /// ([`frozen_warning_text`] to [`super::IdleHost::inform`]) or the
    /// explicit restart under `stall_term = true` (`restart pid=<leader>
    /// if-stalled quiet=<grace>`; an older server, `signal term`). Once per
    /// episode, only where the host relaunches the agent, never under a
    /// person's or another driver's hand, never for a stopped job. A refusal
    /// the next read may lift (`not-stalled`, `busy`, `halted`) is not
    /// counted, so the next read tries again; one it cannot (`restart
    /// unavailable`) is, the warning said instead.
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
            // The default: a warning where the restart would be, once per
            // episode: said, and one record for the person (the Messages).
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
        // THE EXPLICIT RESTART (design of 2026-10-08): the server stamps the
        // agent before it signals, so its exit is the restart's whoever
        // reads it, and its own fences decide under the same read.
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
            // An older server: the bare `signal term` it knows. It answers a
            // verb it does not know `ERR unknown verb` — or, aimed at a
            // session as this one is (`@<sid>`), `ERR denied`: an unknown
            // verb has no op class to authorize (review of 3eaa8f83c).
            return self.stall_signal_term(&stall, seq, review);
        }
        let reply = reply_line(&r);
        if r.is_err("restart unavailable") {
            // The server cannot carry the restart here: counted, and the
            // warning said instead, naming what a person can do.
            self.stall_termed = true;
            let after = self.stall_host.as_ref().map_or(AfterRestart::Unknown, |h| {
                h.resume_command()
                    .map_or(AfterRestart::Unknown, AfterRestart::Command)
            });
            return self.warn_stall(read, &stall, &after, seq, review, &format!("({reply})"));
        }
        // `ERR not-stalled`, `busy`, `halted`: not counted, the next read
        // decides.
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

    /// The stall's warning in place of its remedy (once an episode): said
    /// `STALL-WARNED … ended=no <why>`, and the record for a person
    /// ([`frozen_warning_text`] with `after`) told the host.
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
        let text = frozen_warning_text(stall, program, self.sid.as_deref(), after, leader);
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

    /// The remedy an older server knows (no `restart` verb): the bare `signal
    /// term`, counted once it is sent.
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

    /// After a step of [`Self::wait_for_next`] that did not latch: a probe,
    /// unless a read has shown the host does not measure the input and no
    /// stall is held or suspected ([`Caps::input`]: not known yet probes). A
    /// stall seen for the first time is held ([`Self::hold_for_stall`]) and
    /// the wait goes on; `true` when a held stall has LIFTED — the wait ends
    /// and the next look thaws it ([`Self::stall_look`]).
    pub(super) fn stall_step(&mut self, seq: u64, review: &mut dyn Review) -> Result<bool, Fail> {
        if !review.unattended()
            || (self.caps.input == Some(false) && self.stalled.is_none() && !self.stall_suspect)
        {
            return Ok(false);
        }
        let read = self.stall_probe()?;
        // This read is the fresh one: it decides whatever an earlier one
        // suspected.
        self.stall_suspect = false;
        let Some(read) = read else {
            return Ok(self.stalled.is_some());
        };
        if read.stall.is_none() {
            return Ok(self.stalled.is_some());
        }
        if self.stalled.is_some() {
            // Held already: the numbers move, the episode does not — and the
            // remedy is looked at again on them.
            self.stalled.clone_from(&read.stall);
            self.stall_remedy(&read, seq, review)?;
        } else {
            self.hold_for_stall(read, seq, review)?;
        }
        Ok(false)
    }

    /// The top of a look, while a stall is held or a press was refused `ERR
    /// busy input-unread` (or written with its warning): `status` is read
    /// before anything is pressed. `true` holds the look (nothing pressed,
    /// typed or escalated; the caller waits); `false` goes on, after
    /// [`Self::thaw`] when a held stall has lifted.
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
        // Negative controls: a measured word that is no stall, the unmeasured
        // `-`, a host with no field, an ERR.
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

    #[test]
    fn the_frozen_mail_names_the_wait_the_backlog_the_size_and_the_remedy() {
        let s = worker_stall(STALLED).expect("stall");
        // The worker's own conversation as the host read it, never
        // `claude --continue`.
        let own = AfterRestart::Command(
            "claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f".to_string(),
        );
        assert_eq!(
            frozen_mail_text(&s, "claude", &own, Some("@s-1"), Some(4242)),
            "claude frozen: not reading input for 2m41s (1 B queued, rss 38.8 GB); pressing \
             nothing until it reads again \u{2014} restart it: signal term (signal kill if it \
             survives), then claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f"
        );
        // The host relaunches it itself: no command for the manager to run.
        let hosted = frozen_mail_text(
            &s,
            "claude",
            &AfterRestart::Relaunch,
            Some("@s-1"),
            Some(4242),
        );
        assert!(
            hosted.ends_with(
                "restart it: aterm ctl @s-1 restart pid=4242 if-stalled; aterm relaunches it on \
                 its conversation"
            ),
            "{hosted}"
        );
        // A program with no resume command, or a Claude Code whose record
        // was not read: the restart alone — never a guess.
        let t = frozen_mail_text(&s, "vim", &AfterRestart::Unknown, Some("@s-1"), Some(4242));
        assert!(
            t.ends_with("restart it: signal term (signal kill if it survives)"),
            "{t}"
        );
        // A small program reads MB, never `0.0 GB`; unknown numbers are left
        // out, never printed as `-`.
        let small = WorkerStall {
            rss_mb: Some(12),
            wait_ms: None,
            ..s.clone()
        };
        assert_eq!(
            frozen_mail_text(
                &small,
                "claude",
                &AfterRestart::Unknown,
                Some("@s-1"),
                Some(4242)
            ),
            "claude frozen: not reading input (1 B queued, rss 12 MB); pressing nothing until \
             it reads again \u{2014} restart it: signal term (signal kill if it survives)"
        );
        let stopped = WorkerStall {
            word: "stopped",
            rss_mb: None,
            wait_ms: Some(41_000),
            ..s
        };
        assert_eq!(
            frozen_mail_text(&stopped, "claude", &own, Some("@s-1"), Some(4242)),
            "claude stopped with input queued for 41s (1 B queued); pressing nothing until it \
             reads again \u{2014} resume it: signal cont"
        );
    }

    #[test]
    fn a_wait_reads_as_the_band_reads_one() {
        assert_eq!(wait_text(999), "0s");
        assert_eq!(wait_text(41_000), "41s");
        assert_eq!(wait_text(123_000), "2m03s");
        assert_eq!(wait_text(9_660_000), "2h41m");
        assert_eq!(wait_text(97_200_000), "1d 3h");
    }
}
