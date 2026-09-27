// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's hold on a worker that has stopped reading its input.
//!
//! **The incident (2026-09-24).** A Claude Code worker grew to 38.8 GiB and
//! stopped reading its terminal. An Enter already sat in its input queue.
//! Its screen still showed the question box it drew before it froze, so the
//! hosted loop escalated that box and badged the session "answer this box"
//! (`claude other: … (a other box: no rule approves this kind)`), and then
//! waited on `await gone <the box>` in 20 s steps. The box never left the
//! screen, because the program that would have removed it was reading
//! nothing. For 2h41m the menu bar told a human to answer a box that no
//! keystroke could reach: every key they, or anyone, sent would have been
//! queued behind that Enter and read later against a screen the program had
//! not drawn.
//!
//! **What the server says now.** aterm dates every input byte the kernel
//! accepts, and `status` publishes how long the oldest unread one has waited
//! (`input=clear|pending|typeahead|stalled|stopped input_bytes=<n>
//! input_wait_ms=<ms> fg_rss_mb=<n>`). At 10 s it publishes the stall
//! itself: `agent=wall:unresponsive` and a keyed attention entry owned by
//! `aterm` ("claude is frozen: not reading input since …"), cleared by the
//! probe that sees the input read again. A socket key sent behind input that
//! has waited a second is refused `ERR busy input-unread …`.
//!
//! **What this loop does with it.** The server's entry IS the badge, so the
//! supervisor writes no frozen badge of its own — one would go stale after
//! recovery and be notified on. On `status input=stalled|stopped` the loop
//! ([`Session::hold_for_stall`]):
//!
//! * unsets its own point badge ([`Session::unset_if_ours`], any entry of
//!   its own that is not a limit's), so no "answer this box" stands under
//!   the server's "frozen";
//! * forgets the escalated box ([`Session::box_ask`]) and the turn-end
//!   policy's due decision ([`Session::turn_end_due`]);
//! * presses, types and escalates nothing while the stall stands;
//! * posts ONE `kind=ask` to the manager per episode ([`frozen_mail_text`]),
//!   only while `status` says `fabric=connected` — the same rule as every
//!   escalation's mail;
//! * journals `FROZEN seq=<n> input=<w> wait_ms=<ms> bytes=<n> rss_mb=<n|->
//!   attention=<unset reply> mail=<post word>`.
//!
//! When `status` reads anything else again ([`Session::thaw`]) it journals
//! `THAWED seq=<n> input=<w>` and forgets the point it handed over, so the
//! point still showing is decided afresh: at full power, answered.
//!
//! **Where it looks.** [`Session::stall_step`] runs after every step of the
//! wait for the screen to move that did not latch ([`Session::wait_for_next`])
//! — the wait the incident's loop sat in. [`Session::stall_look`] runs at the
//! top of a look while a stall is held, or after a press was refused `ERR
//! busy input-unread` ([`Session::back_off`] sets
//! [`Session::stall_suspect`]): the refusal fires at 1 s and the stall is
//! published at 10 s, so the loop reads `status` before it presses again.
//!
//! **The read in hand.** Every `status` the loop reads for its own reasons —
//! the box's program, the program before a turn-end continuation, the
//! escalation's fabric, a halt — is read for a stall too
//! ([`Session::note_status`]), and one it reports is acted on at once, never
//! 20 s later at the wait's next step (review finding on this slice,
//! 2026-09-25: a loop that started over a worker already frozen read
//! `input=stalled` in the status that named the box's program, and then
//! pressed the box, or badged it "answer this box" and asked the manager
//! to). Such a read suspects the stall, and while it is suspected
//! ([`Session::stall_in_hand`]) a box is neither pressed nor escalated — the
//! look goes round and its [`Session::stall_look`] holds
//! ([`Session::auto_read`]) — the turn-end policy types nothing
//! ([`Session::turn_end_execute`]), an escalation raises no badge and mails
//! no ask ([`Session::withhold_badge`], [`ASK_WITHHELD`]), and the wait holds
//! before its first step ([`Session::stall_before_wait`]).
//!
//! **A point with no `status` read before its badge** (a question the worker
//! asked, an idle point the turn-end policy escalates). With a manager, the
//! escalation's read of the fabric comes after the badge: it is the first
//! to say, and the badge stands for that one round trip. With none — the
//! window's host, which never has one, the incident's host — nothing was
//! read at all, and a worker frozen under its own question kept its badge
//! for the whole stall (review finding on this slice, 2026-09-25): such an
//! escalation now reads `status` before its badge, for this alone
//! ([`Session::withhold_badge`]), and a stall it reports raises none.
//!
//! **Hosts that do not say.** The wait probes until a `status` reply shows
//! the host does not measure the input ([`Caps::input`], learned from every
//! `status` the loop reads anyway: [`Session::note_status`]) — not knowing
//! yet is no reason not to ask, and a loop that read no `status` before its
//! first wait would otherwise never ask at all (the same finding). A host
//! without the field — an older aterm, or one not on macOS, where it reads
//! `input=-` — sees at most one `status` more than it saw before, the read
//! that learns so (once per connection: an outage forgets what a host said),
//! and from then on exactly the requests it saw before.

use super::escalate::status_field;
use super::*;

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
/// the stall when that word is one, and the two fields the hold needs from
/// the same read (`fabric=`, `program=`).
pub(super) struct StallRead {
    word: String,
    stall: Option<WorkerStall>,
    fabric: String,
    program: Option<String>,
}

/// The mail word of an escalation whose own `status` read reported the
/// stall ([`Session::escalate`]): an ask to answer a point no answer can
/// reach is not mailed; the hold's own mail tells the manager what is
/// wrong ([`frozen_mail_text`]). The attention word too, of an ask whose
/// badge a stall in hand kept down ([`Session::withhold_badge`]).
pub(super) const ASK_WITHHELD: &str =
    "skipped: the same status says the worker is not reading its input";

/// Whether a `status` reply carries a MEASURED `input=` word: a host that
/// publishes the backlog, on a session it can measure. An absent field (an
/// older aterm) and `input=-` (not macOS, not a tty) are both no.
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

/// The one mail an episode posts to the manager:
///
/// ```text
/// claude frozen: not reading input for 2m41s (1 B queued, rss 38.8 GB); pressing nothing until it reads again — restart it: signal term (signal kill if it survives), then claude --continue
/// claude stopped with input queued for 41s (1 B queued); pressing nothing until it reads again — resume it: signal cont
/// ```
///
/// `program` names the worker (`status program=`, else Claude Code's
/// `claude`); `resume` is the command that resumes its conversation after a
/// restart ([`aterm_phase::resume_hint`]), `None` for a program with none.
/// The restart names its fallback: a program can live through `signal term`
/// — a Node program's SIGTERM listener never runs while its JS thread spins
/// (whole-branch review, third round, 2026-09-25) — and then only `signal
/// kill` ends it. The server keeps the stall published meanwhile.
pub(super) fn frozen_mail_text(stall: &WorkerStall, program: &str, resume: Option<&str>) -> String {
    let waited = stall
        .wait_ms
        .map_or_else(String::new, |ms| format!(" for {}", wait_text(ms)));
    let facts: Vec<String> = stall
        .bytes
        .map(|n| format!("{n} B queued"))
        .into_iter()
        .chain(stall.rss_mb.map(rss_text))
        .collect();
    let facts = if facts.is_empty() {
        String::new()
    } else {
        format!(" ({})", facts.join(", "))
    };
    if stall.word == "stopped" {
        return format!(
            "{program} stopped with input queued{waited}{facts}; pressing nothing until it reads \
             again \u{2014} resume it: signal cont"
        );
    }
    let resume = resume.map_or_else(String::new, |r| format!(", then {r}"));
    format!(
        "{program} frozen: not reading input{waited}{facts}; pressing nothing until it reads \
         again \u{2014} restart it: signal term (signal kill if it survives){resume}"
    )
}

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

    /// Before an ask's badge goes up ([`Self::escalate`]): `true` when a
    /// stall is in hand ([`Self::stall_in_hand`]), so the badge — "answer
    /// this" over a program no answer can reach — is not raised and nothing
    /// is mailed. It is journaled `ESCALATED seq=<n> attention=… mail=…`,
    /// both words [`ASK_WITHHELD`] (the mail's, with no manager, the reason
    /// there is none). The hold that follows before the wait's first step
    /// ([`Self::stall_before_wait`]) tells the manager what is wrong.
    ///
    /// **With no manager — the window's host never has one — the escalation
    /// reads `status` here, for this alone.** An escalation with a manager
    /// reads `status` after its badge for the fabric, and that read tells the
    /// loop of a stall ([`Self::note_status`]; the badge stands one round
    /// trip, [`ASK_WITHHELD`] withholds the ask). One with no manager read
    /// nothing, so a point badged with no `status` read before it — a
    /// question the worker asked, an idle point the turn-end policy escalates
    /// — kept "answer this" over a worker frozen under it for the whole stall,
    /// and the wait, having never seen a measured `input=`, never probed
    /// (review finding on this slice, 2026-09-25, reproduced over the
    /// scripted server as the hosted loop builds it: the badge, then `await
    /// seq` steps and no `status`, no unset, no FROZEN). The read is skipped
    /// on a host a read has shown measures nothing ([`Caps::input`]), and
    /// while a stall is already suspected.
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
            "skipped: no --inbox and no $ATERM_PARENT_SESSION_ID"
        } else {
            ASK_WITHHELD
        };
        review.note(&format!(
            "ESCALATED seq={seq} attention={ASK_WITHHELD} mail={mail}"
        ));
        Ok(true)
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
        }))
    }

    /// A stall seen: hold. The loop's own point badge is unset (the server's
    /// `aterm` entry says "frozen" over it, and would leave ours standing as
    /// "answer this box" the moment it cleared), the escalated box and the
    /// policy's due decision are forgotten, one `kind=ask` goes to the
    /// manager while the fabric is connected, and the episode is journaled
    /// `FROZEN …`. Nothing is pressed or typed here or while it stands.
    pub(super) fn hold_for_stall(
        &mut self,
        read: StallRead,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(stall) = read.stall else {
            return Ok(());
        };
        self.stall_suspect = false;
        self.box_ask = None;
        self.adopted_box = false;
        self.turn_end_due = None;
        let behind = self.claim.watching_behind().map(str::to_string);
        let (attention, mail) = if let Some(holder) = behind {
            let skipped = format!("skipped: another supervisor ({holder}) answers this session");
            (skipped.clone(), skipped)
        } else {
            let attention = self.unset_if_ours(|t| !t.starts_with(ATTENTION_PREFIX))?;
            if self.attention_ours.is_none() {
                // The turn-end policy's badge, if that was ours, went too.
                self.turn_end_badge = false;
            }
            let mail = match self.manager.clone() {
                None => "skipped: no --inbox and no $ATERM_PARENT_SESSION_ID".to_string(),
                Some(to) if read.fabric == "connected" => {
                    let rows = self.last.as_ref().map_or(&[][..], |s| s.rows.as_slice());
                    let reader = aterm_phase::identify(read.program.as_deref(), rows).program();
                    let program = read.program.as_deref().unwrap_or(reader.name());
                    let text = frozen_mail_text(&stall, program, aterm_phase::resume_hint(reader));
                    let to = format!("to={to}");
                    let r = self.call(&["post", &to, "kind=ask", "--wait=0", &text])?;
                    post_word(&r)
                }
                Some(_) => format!("skipped: no fabric (fabric={})", read.fabric),
            };
            (attention, mail)
        };
        let dash = || "-".to_string();
        review.note(&format!(
            "FROZEN seq={seq} input={} wait_ms={} bytes={} rss_mb={} attention={attention} \
             mail={mail}",
            stall.word,
            stall.wait_ms.map_or_else(dash, |v| v.to_string()),
            stall.bytes.map_or_else(dash, |v| v.to_string()),
            stall.rss_mb.map_or_else(dash, |v| v.to_string()),
        ));
        self.stalled = Some(stall);
        if let Some(host) = &self.stall_host {
            host.stalled(true);
        }
        Ok(())
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
        if self.stalled.take().is_some() {
            review.note(&format!("THAWED seq={seq} input={}", or_dash(word)));
            state.handed = None;
            if let Some(host) = &self.stall_host {
                host.stalled(false);
            }
        }
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
            // Held already: the numbers move, the episode does not.
            self.stalled = read.stall;
        } else {
            self.hold_for_stall(read, seq, review)?;
        }
        Ok(false)
    }

    /// The top of a look, while a stall is held or a press was refused `ERR
    /// busy input-unread`: `status` is read before anything is pressed.
    /// `true` holds the look — nothing pressed, typed or escalated, and the
    /// caller waits as it waits on any point; `false` goes on, after
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
                    self.stalled = read.stall;
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

    /// The incident's reading, as `status` spells it (plan §3).
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
        // NEGATIVE CONTROLS: a measured word that is no stall, the
        // unmeasured `-`, a host with no field, an ERR.
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
        assert_eq!(
            frozen_mail_text(&s, "claude", Some("claude --continue")),
            "claude frozen: not reading input for 2m41s (1 B queued, rss 38.8 GB); pressing \
             nothing until it reads again \u{2014} restart it: signal term (signal kill if it \
             survives), then claude --continue"
        );
        // A program with no resume command: the restart alone.
        let t = frozen_mail_text(&s, "vim", None);
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
            frozen_mail_text(&small, "claude", None),
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
            frozen_mail_text(&stopped, "claude", Some("claude --continue")),
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
