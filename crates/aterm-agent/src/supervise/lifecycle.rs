// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a supervisor lives with a session it does not own: a refusal is waited out rather than ending the watch, a badge a previous watcher left is settled at the start, the badges this one raised go with it, and the in-GUI host runs the same loop under a stop flag.

use super::super::journal::{
    ADOPTED, AGAIN_UNPARKED, ATTENTION_SKIPPED, CLEARED_HIDDEN, CLEARED_STALE, ESCALATED_BEHIND,
};
use super::escalate::{NEEDS_LEFT, kinds_name_owner, point_label, status_field};
use super::*;

/// The `<why>` of the `CLEARED seq=<n> attention=<said> <why>` line that takes
const PARKED_OFF: &str = "the session's autopilot is off";

/// The journal's note AN OFF PARK STARTS WITH ([`Session::park_while_off`]): `PARKED seq=<n> autopilot=off: …`.
pub(super) fn parked_off_note(seq: u64) -> String {
    format!(
        "PARKED seq={seq} autopilot=off: the harness keeps its hands off this session \
         (nothing pressed, typed or escalated) until its word changes"
    )
}

/// Whether journal line `line` is [`parked_off_note`]'s.
pub(super) fn is_parked_off(line: &str) -> bool {
    line.strip_prefix("PARKED seq=")
        .and_then(|rest| rest.split_once(' '))
        .is_some_and(|(seq, rest)| {
            seq.bytes().all(|b| b.is_ascii_digit()) && rest.starts_with("autopilot=off")
        })
}

/// What makes two refusals the same for the journal ([`Session::refusal_news`]): the words, less every field the server measures anew at each retry, a `<name>=<number>` whose name counts time or bytes (`human_ms=<n>`, `bytes=<n>`, a `…_ms=` wait).
pub(super) fn refusal_key(said: &str) -> String {
    said.split_whitespace()
        .filter(|word| {
            !word.split_once('=').is_some_and(|(name, value)| {
                (name.ends_with("_ms") || name.ends_with("bytes"))
                    && !value.is_empty()
                    && value.bytes().all(|b| b.is_ascii_digit())
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl<C: Ctl> Session<'_, C> {
    /// The hosted loop's stop flag is set: read through the door ([`super::super::fx::Fx::raised`]), so a harness tape holds the read that first saw it raised — and how many reads before it did not — in the order the loop read it, whichever thread raised it.
    pub(super) fn stopped(&self) -> bool {
        self.fx.raised(super::super::tape::doors::RAISED_STOP, || {
            self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst))
        })
    }

    /// Record the session's working directory from a bare `meta` reply (`cwd=<pct>`; `-` is unknown).
    pub(super) fn note_cwd(&mut self, meta: &str) {
        let Some(line) = meta.lines().find(|l| crate::is_ok_reply(l)) else {
            return;
        };
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .filter(|v| *v != "-" && !v.is_empty())
                .map(super::super::report::pct_decode)
                .filter(|p| p.starts_with('/') || std::path::Path::new(p).is_absolute())
                .map(PathBuf::from)
        };
        self.cwd = field("agent_cwd=").or_else(|| field("cwd="));
        self.trusted_cwd = field("trusted_cwd=");
        self.worker_agent = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("worker="))
            .and_then(|w| {
                aterm_phase::Program::ALL
                    .into_iter()
                    .find(|p| p.name() == w && *p != aterm_phase::Program::Generic)
            });
    }

    /// A press or a dismissal the server did not take: parked on the hold (`halted`), else backed off.
    pub(super) fn refused(
        &mut self,
        halted: bool,
        why: &str,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if halted {
            self.park_on_hold(why, seq, review)
        } else {
            self.back_off(why, seq, None, review)
        }
    }

    /// Whether the refusal `said` (`REFUSED <why>`, `HALTED <why>`), met at `now`, is news for the journal: not the one it last said ([`refusal_key`]), or that one met again after [`REFUSAL_QUIET`] unmet.
    pub(super) fn refusal_news(&mut self, said: &str, now: Instant) -> bool {
        let key = refusal_key(said);
        let news = !self.refusal_said.as_ref().is_some_and(|(last, met)| {
            *last == key && now.saturating_duration_since(*met) < REFUSAL_QUIET
        });
        self.refusal_said = Some((key, now));
        news
    }

    /// `ERR halted`: wait for the hold to lift.
    pub(super) fn park_on_hold(
        &mut self,
        why: &str,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let why = clip(&one_line(why));
        if self.refusal_news(&format!("HALTED {why}"), self.fx.mono()) {
            review.note(&format!("HALTED seq={seq} {why}"));
        }
        let since = self.fx.mono();
        loop {
            if self.stopped() {
                return Ok(());
            }
            let st = self.call(&["status"])?;
            if self.unserved(&st) {
                return Err(Fail::Lost(format!("status failed: {}", st.stderr.trim())));
            }
            let hold = if st.ok() {
                status_field(&st.stdout, "hold")
            } else {
                None
            };
            match hold {
                Some("0") => {
                    review.note(&format!(
                        "UNHALTED seq={seq} after {} ms",
                        self.fx.since(since).as_millis()
                    ));
                    self.refusal_said = None;
                    return Ok(());
                }
                Some("1") => {
                    let ms = WAIT_STEP.as_millis().to_string();
                    let r =
                        self.call(&["await", "inbox", "since=0", "kinds=hold", "timeout", &ms])?;
                    if self.unserved(&r) {
                        return Err(Fail::Lost(format!(
                            "await inbox failed: {}",
                            r.stderr.trim()
                        )));
                    }
                    if r.ok() || r.timed_out() {
                        continue;
                    }
                    self.wait(&["seq", &seq.to_string()], WAIT_STEP)?;
                    return Ok(());
                }
                _ => {
                    self.wait(&["seq", &seq.to_string()], WAIT_STEP)?;
                    return Ok(());
                }
            }
        }
    }

    /// Autopilot `off` (the session's `autopilot` word, else `[harness] autopilot = "off"`; [`Session::autopilot`]): park as on a hold ([`Self::park_on_hold`]), with nothing pressed, typed or escalated, no host step, no probe and no point reported, until the word changes.
    pub(super) fn park_while_off(
        &mut self,
        seq: u64,
        deadline: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        review.note(&parked_off_note(seq));
        self.lower_badges(seq, review)?;
        self.give_back_claim();
        self.off_stood(seq);
        let since = self.fx.mono();
        let mut seq = seq;
        loop {
            if self.stopped() || self.fx.mono() >= deadline {
                return Ok(());
            }
            let step = deadline
                .saturating_duration_since(self.fx.mono())
                .min(OFF_STEP);
            match self.wait(&["seq", &seq.to_string()], step)? {
                Wait::Latched => {
                    self.wait(&["idle", IDLE_MS], step)?;
                    let screen = self.screen()?;
                    match self.reader(&screen.rows).phase(&screen.rows) {
                        Phase::Busy => self.badge_parked = None,
                        Phase::Limited { .. } => {}
                        _ => {
                            self.retry = None;
                            self.adopted_limit_over(screen.seq, ANSWERED_PAST_LIMIT, review)?;
                        }
                    }
                    seq = screen.seq;
                }
                Wait::TimedOut => {}
                Wait::Unsupported => {
                    return Err(Fail::Hard(
                        "await seq is not known to this host".to_string(),
                    ));
                }
            }
            self.autopilot.begin();
            let now = self.autopilot_read(opts)?;
            if now != Autopilot::Off {
                review.note(&format!(
                    "UNPARKED seq={seq} autopilot={} after {} ms",
                    now.word(),
                    self.fx.since(since).as_millis()
                ));
                self.retake_claim(review);
                self.rejudge = true;
                self.raise_parked_badge(seq, review)?;
                return Ok(());
            }
        }
    }

    /// AUTOPILOT OFF STANDS ON THE SESSION: what the park ends is over ([`TurnEndState::park_off`]), and the ledger says so ([`approvals::RULE_OFF_STOOD`]) for a loop that follows, which takes up its books as this one holds them after the park (`Session::seed_from_ledger`).
    pub(super) fn off_stood(&mut self, seq: u64) {
        self.turn_end.park_off();
        self.tell_hold();
        self.ledger_row(
            approvals::RULE_OFF_STOOD,
            approvals::Outcome::Waiting,
            "",
            approvals::OFF_STOOD,
            seq,
        );
    }

    /// `ERR busy …` / `ERR rate`: wait [`Session::refusal_pause`] (doubling to [`REFUSAL_PAUSE_MAX`]) or until `anchor` leaves the screen, as the server's own wait, never a sleep here.
    pub(super) fn back_off(
        &mut self,
        why: &str,
        seq: u64,
        anchor: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let pause = self.refusal_pause;
        self.refusal_pause = pause.saturating_mul(2).min(REFUSAL_PAUSE_MAX);
        if why.contains("ERR busy input-unread") {
            self.stall_suspect = true;
        }
        let said = clip(&one_line(why));
        if self.refusal_news(&format!("REFUSED {said}"), self.fx.mono()) {
            review.note(&format!(
                "REFUSED seq={seq} backoff={} {said}",
                pause.as_millis()
            ));
        }
        let ms = pause.as_millis().to_string();
        match anchor {
            Some(a) => self.wait(&["gone", a], pause)?,
            None => self.wait(&["idle", &ms], pause)?,
        };
        Ok(())
    }

    /// A watch's first act after its claim: the keyed attention entry a previous supervisor left on the worker (owner `supervisor`) — a watcher that died, the host before a restart — is settled.
    pub(super) fn reconcile(&mut self, review: &mut dyn Review) -> Result<(), Fail> {
        let read = self.call(&["meta"])?;
        if self.unserved(&read) {
            return Err(Fail::Lost(format!("meta failed: {}", read.stderr.trim())));
        }
        if !read.ok() {
            return Ok(());
        }
        self.note_meta(&read.stdout);
        if self.needs_not_ours() {
            return Ok(());
        }
        if self.needs_told.is_none()
            && status_field(&read.stdout, "attention_kinds")
                .is_some_and(|kinds| kinds_name_owner(kinds, NEEDS_ATTENTION_OWNER))
        {
            self.needs_told = Some(NEEDS_LEFT);
        }
        let shown_owner =
            status_field(&read.stdout, "attention_owner").map(super::super::report::pct_decode);
        if shown_owner.as_deref() != Some(ATTENTION_OWNER) {
            let hidden = status_field(&read.stdout, "attention_owners")
                .and_then(|n| n.parse::<usize>().ok())
                .is_some_and(|n| n > 1);
            if hidden {
                let owner = format!("owner={ATTENTION_OWNER}");
                let r = self.call(&["meta", "unset", "attention", &owner])?;
                review.note(&format!(
                    "CLEARED seq=- {CLEARED_HIDDEN} attention={} (an entry of ours may sit \
                     under another owner's)",
                    reply_word(&r)
                ));
            }
            return Ok(());
        }
        let Some(text) = standing_attention(&read.stdout) else {
            return Ok(());
        };
        if super::task_mark::deadline_attention(&text) {
            return Ok(());
        }
        let screen = self.screen()?;
        let turn = self.point_of(screen);
        let showing = match &turn.phase {
            Phase::Limited { .. } => text.starts_with(ATTENTION_PREFIX),
            Phase::Prompt | Phase::Question | Phase::Idle => text.starts_with(&format!(
                "{} (",
                point_label(self.reader(&turn.screen.rows), &turn)
            )),
            _ => false,
        };
        let seq = turn.screen.seq;
        if showing {
            if matches!(turn.phase, Phase::Limited { .. }) {
                self.adopted_limit = true;
            } else {
                self.box_ask = Some(BoxAsk {
                    key: self.review_key(&turn),
                    memory: false,
                });
                self.adopted_box = true;
            }
            self.attention_ours = Some(text.clone());
            review.note(&format!("{ADOPTED} seq={seq} attention={}", clip(&text)));
        } else {
            let owner = format!("owner={ATTENTION_OWNER}");
            let r = self.call(&["meta", "unset", "attention", &owner])?;
            review.note(&format!(
                "CLEARED seq={seq} {CLEARED_STALE} attention={} (its point is not on the screen)",
                reply_word(&r)
            ));
        }
        Ok(())
    }

    /// A watch's last act: the badges it raised — a point's, a limit's, the turn-end policy's, the worker's NEEDS YOU entry ([`Self::needs_down`]) — are cleared if they are still its own, each journaled `CLEARED` (the turn-end policy's `CLEARED seq=<n> turn-end attention=<said>`, so the harness's stats close the person's wait it opened on its own line, not on the loop's end), and the claim is released.
    pub(super) fn release_attention(&mut self, review: &mut dyn Review) {
        let seq = self.last.as_ref().map_or(0, |s| s.seq);
        if self.box_ask.is_some() {
            let _ = self.close_box(seq, review);
        }
        if self.limit.is_some() {
            let _ = self.close_episode(seq, "the watcher ended", review);
        }
        if std::mem::take(&mut self.turn_end_badge)
            && let Ok(said) = self.unset_if_ours(|_| true)
        {
            review.note(&format!(
                "CLEARED seq={seq} {CLEARED_TURN_END} attention={said}"
            ));
        }
        let _ = self.needs_down(seq, "the loop ended", review);
        self.tell_hold_ended();
        self.release_claim();
    }

    /// AN OFF PARK'S FIRST ACT ([`Self::park_while_off`]): the badges this loop holds are taken down as [`Self::release_attention`] takes them down at its end, the claim left to the park's own [`Self::give_back_claim`] — an escalated point's ([`Self::close_box`], `CLEARED seq=<n> box attention=<said>`), a limit episode's ([`Self::close_episode`], `CLEARED seq=<n> attention=<said> <why>`), a limit's adopted from a previous loop whose episode has not opened yet (the same line, while its badge is still the loop's entry), the turn-end policy's (`CLEARED seq=<n> turn-end attention=<said>`), and the worker's NEEDS YOU entry (`CLEARED seq=<n> needs attention=<said> <why>`, [`Self::needs_down`]), which is not kept for after the park.
    pub(super) fn lower_badges(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        self.close_box(seq, review)?;
        if let Some(ep) = self.limit.as_ref().filter(|ep| ep.raised) {
            self.retry = Some(Retry { seq: ep.seq });
        }
        self.close_episode(seq, PARKED_OFF, review)?;
        if self.adopted_limit {
            self.lower_adopted_limit(seq, PARKED_OFF, review)?;
        }
        if std::mem::take(&mut self.turn_end_badge) {
            let text = self.attention_ours.clone();
            let said = self.unset_if_ours(|_| true)?;
            review.note(&format!(
                "CLEARED seq={seq} {CLEARED_TURN_END} attention={said}"
            ));
            self.badge_parked = text;
        }
        self.needs_down(seq, PARKED_OFF, review)?;
        Ok(())
    }

    /// THE TURN-END POLICY'S BADGE SET AGAIN as an Off park ends on a word
    fn raise_parked_badge(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        let Some(text) = self.badge_parked.take() else {
            return Ok(());
        };
        if let Some(holder) = self.claim.watching_behind() {
            review.note(&format!(
                "ESCALATED seq={seq} {ATTENTION_SKIPPED}{ESCALATED_BEHIND} ({holder}) answers \
                 this session"
            ));
            return Ok(());
        }
        if self.withhold_badge(seq, super::escalate::Escalation::Ask, review)? {
            return Ok(());
        }
        let owner = format!("owner={ATTENTION_OWNER}");
        let r = self.call(&["meta", "set", "attention", &owner, &text])?;
        if r.ok() {
            self.attention_ours = Some(text);
            self.turn_end_badge = true;
        }
        review.note(&format!(
            "ESCALATED seq={seq} attention={} again={AGAIN_UNPARKED}",
            reply_word(&r)
        ));
        Ok(())
    }

    /// The in-GUI host's loop: the engine's watching loop (one flushed line per approval and per review point, [`Lines`]), under `opts` (normally [`SuperviseOpts::hosted`]), until `stop` is set.
    pub fn run_hosted(
        &mut self,
        opts: &SuperviseOpts,
        stop: Arc<AtomicBool>,
        out: &mut (dyn Write + Send),
    ) -> Result<(), String> {
        self.stop = Some(stop);
        if !self.stopped() {
            self.claim_at_naming();
        }
        self.tell_own = true;
        if self.setup.ledger_path.is_none() {
            self.setup.ledger_path.clone_from(&opts.ledger);
        }
        let allow = opts.allow();
        let mut warn = std::io::stderr();
        if let Some(path) = opts.journal.as_deref() {
            self.fx.bound(path);
        }
        let journal = self.fx.open_journal(
            opts.journal.as_deref(),
            self.setup.sid.as_deref(),
            &mut warn,
        );
        let sink = Mutex::new(Sink {
            out,
            journal,
            warn: Some(&mut warn),
            fx: self.fx.clone(),
        });
        let mut review = Lines {
            sink: &sink,
            allow: &allow,
        };
        let end = self.drive(opts, &mut review);
        let end = if self.stopped() { Ok(End::Quit) } else { end };
        let handed_over = self.stopped()
            && self
                .fx
                .raised(super::super::tape::doors::RAISED_HANDOVER, || {
                    self.setup.handover.raised()
                });
        if handed_over {
            // The next loop adopts what this one raised: only the claim goes.
            if self.box_ask.is_some() || self.turn_end_badge || self.limit.is_some() {
                review.note("KEPT attention for the loop that takes the session over");
            }
            self.release_claim();
        } else if !session_gone(&end) {
            self.release_attention(&mut review);
        }
        let result = match &end {
            Ok(_) => Ok(()),
            Err(e) => Err(e.clone()),
        };
        let (line, _) = watch_end_line(end);
        let _ = sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(&line);
        result
    }
}
