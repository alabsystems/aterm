// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a supervisor lives with a session it does not own: a refusal is waited
//! out rather than ending the watch, a badge a previous watcher left is
//! settled at the start, the badges this one raised go with it, and the
//! in-GUI host runs the same loop under a stop flag.
//!
//! * `ERR halted …` (an owner or fleet `hold`) parks the loop
//!   ([`Session::park_on_hold`]) on the server's `await inbox … kinds=hold`;
//!   no press is re-sent after it: the loop reads and decides again.
//! * The session's Autopilot `off` (its `autopilot` word, else `[harness]
//!   autopilot`) parks the loop the same way ([`Session::park_while_off`]):
//!   nothing pressed, typed or escalated — nor on the busy reads of a turn
//!   it waits out before it parks ([`Session::await_turn_from`]) — the word
//!   read again at each step, and the screen read and decided afresh once it
//!   is not `off`. The session looks unsupervised meanwhile: the loop's
//!   badges are taken down as the park starts ([`Session::lower_badges`],
//!   each journaled `CLEARED`; the turn-end policy's set again at its end
//!   from the text the park kept), the claim is given back for the park
//!   ([`Session::give_back_claim`]) and taken again at its end, so its
//!   boxes are raised to the menu bar again, once; a Land episode in hand is
//!   over, and the task is carried no more: the person drives meanwhile
//!   ([`TurnEndState::park_off`]).
//! * `ERR busy …` and `ERR rate` back the loop off ([`Session::back_off`]):
//!   it waits for the refused point to leave the screen, bounded by a pause
//!   that doubles from 250 ms to 8 s.
//! * Either is journaled when it is news ([`Session::refusal_news`]): a
//!   refusal that persists is retried, and said once, again when it changes.
//! * At the start ([`Session::take_claim`]) the claim is taken, and
//!   ([`Session::reconcile`]) a keyed attention entry a previous supervisor
//!   left is adopted when the screen still shows its point, else unset. A loop
//!   behind another supervisor's claim settles nothing.
//! * At the end ([`Session::release_attention`]) this loop's badges are
//!   cleared, if still its own, each journaled `CLEARED`, and the claim is
//!   released.

use super::super::journal::{
    ADOPTED, ATTENTION_SKIPPED, CLEARED_HIDDEN, CLEARED_STALE, ESCALATED_BEHIND, MAIL_UNPARKED,
};
use super::escalate::{point_label, status_field};
use super::*;

/// The `<why>` of the `CLEARED seq=<n> attention=<said> <why>` line that takes
/// a limit's badge down as an Off park starts ([`Session::lower_badges`]).
const PARKED_OFF: &str = "the session's autopilot is off";

/// What makes two refusals THE SAME for the journal ([`Session::refusal_news`]):
/// the words, with every field that MEASURES dropped — a `<name>=<number>`
/// whose name counts time or bytes (`ERR busy person human_ms=<n>`, how long
/// ago a person typed; `ERR busy input-unread bytes=<n>`; a `…_ms=` wait):
/// the server says it anew at every retry. Who holds the session
/// (`lease=s-9`, `turn=17`, `supervisor=…`, `reason=… origin=…`) is kept: a
/// new holder is news.
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
    /// The hosted loop's stop flag is set.
    pub(super) fn stopped(&self) -> bool {
        self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst))
    }

    /// Record the session's working directory from a bare `meta` reply
    /// (`cwd=<pct>`; `-` is unknown).
    pub(super) fn note_cwd(&mut self, meta: &str) {
        let Some(line) = meta.lines().find(|l| crate::is_ok_reply(l)) else {
            return;
        };
        // The agent process's OWN directory (`agent_cwd=`, read from the OS)
        // first, else the shell's reported one (OSC 7): where it was launched,
        // which is what `ApprovalCtx::cwd` means and the folder Claude Code's
        // trust dialog asks about. The shell's is where its LAST PROMPT was,
        // and stays there after `cd x && claude`, in a worktree and after the
        // harness's own relaunch (`( cd -- DIR && exec claude --resume … )`):
        // `meta` then says `cwd=…/proj agent_cwd=…/proj/sub`, and a relative
        // `rm` was judged against the wrong folder.
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .filter(|v| *v != "-" && !v.is_empty())
                .map(super::super::report::pct_decode)
                .filter(|p| p.starts_with('/'))
                .map(PathBuf::from)
        };
        self.cwd = field("agent_cwd=").or_else(|| field("cwd="));
    }

    /// A press or a dismissal the server did not take: parked on the hold
    /// (`halted`), else backed off; `anchor` is the pattern whose leaving
    /// ends the back-off early (the refused box's row, the survey's).
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

    /// Whether the refusal `said` (`REFUSED <why>`, `HALTED <why>`), met at
    /// `now`, is NEWS for the journal: not the one it last said
    /// ([`refusal_key`]: its measures dropped), or that one met again after
    /// [`REFUSAL_QUIET`] unmet. A refusal that persists — another driver's
    /// lease, a person typing, a hold on a host whose `status` names none —
    /// meets every retry of the act it refused (decided again a moment
    /// later, `turn_end_loop.rs`'s `write_refused`), and is said when it
    /// first appears and when it changes, never at every retry. The episode
    /// also ends at the first write the server does not refuse
    /// ([`Self::call`]) and at a hold seen lifted.
    pub(super) fn refusal_news(&mut self, said: &str, now: Instant) -> bool {
        let key = refusal_key(said);
        let news = !self.refusal_said.as_ref().is_some_and(|(last, met)| {
            *last == key && now.saturating_duration_since(*met) < REFUSAL_QUIET
        });
        self.refusal_said = Some((key, now));
        news
    }

    /// `ERR halted`: wait for the hold to lift. `status hold=1` is waited
    /// out with `await inbox since=0 kinds=hold` (the hold's transition
    /// latches it), a step at most, then read again; `hold=0` returns. A
    /// host whose `status` names no hold, or that refuses the wait, gets one
    /// step (`await seq`) and the loop looks again — the next press says
    /// whether the halt stands. Journaled `HALTED seq=<n> <why>` when the
    /// refusal is news ([`Self::refusal_news`]) and `UNHALTED seq=<n> after
    /// <ms> ms`.
    pub(super) fn park_on_hold(
        &mut self,
        why: &str,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let why = clip(&one_line(why));
        if self.refusal_news(&format!("HALTED {why}"), Instant::now()) {
            review.note(&format!("HALTED seq={seq} {why}"));
        }
        let since = Instant::now();
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
                        since.elapsed().as_millis()
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
                    // The wait is refused on this host: a step, then look.
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

    /// AUTOPILOT OFF (the session's `autopilot` word `off`, else `[harness]
    /// autopilot = "off"`; [`Session::autopilot`]): the loop parks as on a
    /// hold ([`Self::park_on_hold`]) — nothing pressed, typed or escalated, no
    /// host step, no probe, no point reported — until the word changes. Each
    /// step waits for the screen to move past `seq` (at most [`OFF_STEP`]) and,
    /// where it moved, to hold still ([`IDLE_MS`], so a worker that streams is
    /// read at its pauses, never at every frame), then reads the word again
    /// (one bare `meta`, [`Self::autopilot_read`]; on a host that keeps no
    /// word nothing is asked, and the table's `off` stands until the loop is
    /// stopped). Returns once the word is not `off`, at the stop flag, or at
    /// `deadline` (the budget's TIMEOUT is the next look's). Journaled
    /// `PARKED seq=<n> autopilot=off …` as it parks and `UNPARKED seq=<n>
    /// autopilot=<word> after <ms> ms` as it ends on a word; the caller reads
    /// the screen afresh after it.
    ///
    /// OFF LOOKS UNSUPERVISED. The loop's badges are taken down as it parks
    /// ([`Self::lower_badges`], each journaled `CLEARED`, as at the loop's
    /// end): the menu bar raises the session's boxes itself while it is
    /// parked, so a badge of the loop's left standing would tell the person
    /// twice, and the harness's stats would count the whole park as a
    /// person's wait. The supervisor claim is given back
    /// ([`Self::give_back_claim`]: `meta unset supervisor holder=<this
    /// loop>`, the loop's own release) and nothing renews or retries it while
    /// parked, so `status supervisor=` names nobody and the server raises the
    /// session's boxes to the menu bar again, as for any session nobody
    /// supervises; it is taken again as the word turns ([`Self::take_claim`],
    /// whatever it finds — another holder's claim included). Then the
    /// turn-end policy's badge the park took down is set again from the text
    /// it kept ([`Self::raise_parked_badge`]) unless a read of the park saw
    /// the worker working — that badge's end — and the point decided afresh
    /// raises a badge again where its point still shows: a box's or a
    /// question's anew, a limit's as the same episode with no second mail
    /// (unless a read of the park saw the worker answer past the notice: a
    /// new episode then), and an act's own miss, where its act is tried again
    /// and misses again, as a second ask over the turn-end badge set again. A
    /// park the stop flag or the budget ends leaves the claim given back and
    /// the badges down: the loop is ending. And a LAND EPISODE in hand is over
    /// ([`TurnEndState::park_off`]): a Land chosen after the park is a new
    /// one, its instruction typed again — and the task is CARRIED NO MORE: a
    /// person's own exchange a busy read sees after the park (answered within
    /// `EXCHANGE_WORK`) is theirs, never a check-in on a run the harness
    /// drives. The point the park ends on, its turn never seen run, is
    /// decided as before it.
    ///
    /// The harness's own state is cleaned up BEFORE a park, never stranded by
    /// one: a `/usage` panel owed its close gets its one plain close first at
    /// the top of each look ([`Self::look`], [`Self::usage_panel_look`]; one
    /// taken up from the ledger after its grace). What a park leaves
    /// standing is the design's §7
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md`).
    pub(super) fn park_while_off(
        &mut self,
        seq: u64,
        deadline: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        review.note(&format!(
            "PARKED seq={seq} autopilot=off: the harness keeps its hands off this session \
             (nothing pressed, typed or escalated) until its word changes"
        ));
        self.lower_badges(seq, review)?;
        self.give_back_claim();
        self.turn_end.park_off();
        let since = Instant::now();
        let mut seq = seq;
        loop {
            if self.stopped() || Instant::now() >= deadline {
                return Ok(());
            }
            let step = deadline
                .saturating_duration_since(Instant::now())
                .min(OFF_STEP);
            match self.wait(&["seq", &seq.to_string()], step)? {
                Wait::Latched => {
                    self.wait(&["idle", IDLE_MS], step)?;
                    let screen = self.screen()?;
                    match self.reader(&screen.rows).phase(&screen.rows) {
                        // The worker read working: the turn-end policy's
                        // badge the park took down is over (its end outside
                        // a park too), and is not set again.
                        Phase::Busy => self.badge_parked = None,
                        // The notice still up: the limit's episode is kept.
                        Phase::Limited { .. } => {}
                        // The worker answered on a point that is not the
                        // notice, as at a look: the limit's episode the park
                        // kept is over for good ([`Retry`]), and so is a
                        // limit adopted from a previous loop — a limit met
                        // after it is a new one, mailed.
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
                    since.elapsed().as_millis()
                ));
                // The session is supervised again: the claim the park gave
                // back is taken before anything is pressed, typed or raised.
                self.retake_claim(review);
                // A point handed before the park is decided again under the
                // word now standing.
                self.rejudge = true;
                self.raise_parked_badge(seq, review)?;
                return Ok(());
            }
        }
    }

    /// `ERR busy …` / `ERR rate`: wait [`Session::refusal_pause`] (doubling
    /// to [`REFUSAL_PAUSE_MAX`]) or until `anchor` leaves the screen, as the
    /// server's own wait — never a sleep here — then the loop looks again.
    /// Journaled `REFUSED seq=<n> backoff=<ms> <why>` when the refusal is
    /// news ([`Self::refusal_news`]).
    pub(super) fn back_off(
        &mut self,
        why: &str,
        seq: u64,
        anchor: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let pause = self.refusal_pause;
        self.refusal_pause = pause.saturating_mul(2).min(REFUSAL_PAUSE_MAX);
        // The server refused it behind input the worker has not read — a
        // published stall or a stopped job: the next look reads `status`
        // before it presses (`stall.rs`).
        if why.contains("ERR busy input-unread") {
            self.stall_suspect = true;
        }
        let said = clip(&one_line(why));
        if self.refusal_news(&format!("REFUSED {said}"), Instant::now()) {
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

    /// A watch's first act after its claim: the keyed attention entry a
    /// previous supervisor left on the worker (owner `supervisor`) — a
    /// watcher that died, the host before a restart — is settled. When the screen still shows its point (the same box or
    /// question, by [`point_label`]; a limit notice under a `limited:` text)
    /// it is ADOPTED: the point counts as escalated, so no second badge or
    /// mail goes out for it. Otherwise it is unset, by its key. The server
    /// shows only the most recent entry: one of ours under a person's is
    /// unset unread. Another owner's entry is never touched. Journaled
    /// `ADOPTED seq=<n> attention=<text>` or `CLEARED seq=<n> stale
    /// attention=<reply>`. The `meta` read also gives the session's cwd and
    /// its autopilot word ([`Self::note_meta`]): a word `off` parks the first
    /// look. That much is read behind another supervisor's claim too, which
    /// settles nothing: a loop that never read the word would take the host
    /// for one that keeps none, so a park on the table's `off` would ask it
    /// nothing again and never end on the session's word.
    pub(super) fn reconcile(&mut self, review: &mut dyn Review) -> Result<(), Fail> {
        let read = self.call(&["meta"])?;
        if self.unserved(&read) {
            return Err(Fail::Lost(format!("meta failed: {}", read.stderr.trim())));
        }
        if !read.ok() {
            return Ok(());
        }
        self.note_meta(&read.stdout);
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        let shown_owner =
            status_field(&read.stdout, "attention_owner").map(super::super::report::pct_decode);
        if shown_owner.as_deref() != Some(ATTENTION_OWNER) {
            // Only the most recent entry is shown: an entry of ours under a
            // person's cannot be read, so it is cleared unread (its point,
            // if it still shows, is escalated afresh).
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
        let screen = self.screen()?;
        let turn = self.turn_of(screen);
        // An idle point the turn-end policy escalated (`claude idle: …`,
        // `claude wall: …`) is adopted like a box or a question: a loop that
        // restarted over it decides nothing new there (no work seen since),
        // so a badge cleared here would be raised by no one.
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

    /// A watch's last act: the badges it raised — a point's, a limit's, the
    /// turn-end policy's — are cleared if they are still its own, each
    /// journaled `CLEARED` (the turn-end policy's `CLEARED seq=<n> turn-end
    /// attention=<said>`, so the harness's stats close the person's wait it
    /// opened on its own line, not on the loop's end), and the claim is
    /// released. A session already gone answers nothing and nothing is said.
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
        self.release_claim();
    }

    /// AN OFF PARK'S FIRST ACT ([`Self::park_while_off`]): the badges this
    /// loop holds are taken down as [`Self::release_attention`] takes them
    /// down at its end, the claim left to the park's own
    /// [`Self::give_back_claim`] — an escalated point's ([`Self::close_box`],
    /// `CLEARED seq=<n> box attention=<said>`), a limit episode's
    /// ([`Self::close_episode`], `CLEARED seq=<n> attention=<said> <why>`),
    /// a limit's adopted from a previous loop whose episode has not opened
    /// yet (the same line, while its badge is still the loop's entry), and
    /// the turn-end policy's (`CLEARED seq=<n> turn-end attention=<said>`).
    ///
    /// What the point decided afresh after the park would not say again is
    /// kept for it. A box's or a question's point escalates again where it
    /// still shows. A LIMIT is one episode across the park: a raised
    /// episode's close keeps it as a [`Retry`] (as a busy read's does,
    /// `run.rs`'s `limit_on_busy`), and an adopted badge keeps its marker,
    /// so the notice still up after the park sets the badge again with no
    /// second mail (`mail=skipped: the retry hit the wall again …` or
    /// `… adopted …`), which the harness's stats count as no second
    /// escalation. Each is kept only until a read shows the worker answered
    /// on a point that is not the notice — a read of the park, or a look
    /// after it ([`Self::adopted_limit_over`]) — so a limit met after that is
    /// a new episode, mailed. The TURN-END POLICY'S badge mostly carries no
    /// point the rejudge raises it again from — `/login`'s "finish sign-in",
    /// a switch's note, a Codex sandbox fall, each said once — so its text is
    /// kept ([`Session::badge_parked`]) and set again as the park ends on a
    /// word ([`Self::raise_parked_badge`]), unless the worker is read working
    /// meanwhile, which is that badge's end as it is outside a park. One an
    /// act's own miss raised (a text the guard found no row for, one left
    /// typed, a draft with no fenced Enter) is kept and set again the same
    /// way, and its act, decided again after the park, may miss again and
    /// escalate it once more. The note that Codex's goal runs on stays up
    /// with its save-then-wait switch, which a park leaves standing (the
    /// design's §7): an entry that holds it is `kept`. A request not served
    /// is the outage it is.
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
            let text = self
                .attention_ours
                .clone()
                .filter(|t| !t.contains(GOAL_NOTE_HEAD));
            let said = self.unset_if_ours(|t| !t.contains(GOAL_NOTE_HEAD))?;
            review.note(&format!(
                "CLEARED seq={seq} {CLEARED_TURN_END} attention={said}"
            ));
            self.badge_parked = text;
        }
        Ok(())
    }

    /// THE TURN-END POLICY'S BADGE SET AGAIN as an Off park ends on a word
    /// ([`Self::park_while_off`], after the claim is taken again): the text
    /// [`Self::lower_badges`] kept ([`Session::badge_parked`]) is set as
    /// this loop's entry again — no mail (the ask went when it was first
    /// raised), journaled `ESCALATED seq=<n> attention=<reply>
    /// mail=skipped: set again after an off park` ([`MAIL_UNPARKED`]), which
    /// the harness's stats read as a badge up again, not a second
    /// escalation — and cleared as before, once the worker works. Every
    /// turn-end badge is set again so, whether or not a point raises it
    /// again: a badge an act's own miss raised (the guard found no row, a
    /// text left typed, a draft with no fenced Enter) is set here too, and
    /// the act, decided again at the point after the park, may miss the same
    /// way and escalate once more — one more `meta set attention`, its own
    /// `ESCALATED` line and ask after this one, which the stats count as the
    /// escalation it is. Behind another supervisor's claim, or with a stall's
    /// ask withheld, nothing is set, as for any escalation
    /// ([`Self::escalate`]).
    fn raise_parked_badge(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        let Some(text) = self.badge_parked.take() else {
            return Ok(());
        };
        if let Some(holder) = self.claim.watching_behind() {
            review.note(&format!(
                "ESCALATED seq={seq} {ATTENTION_SKIPPED}{ESCALATED_BEHIND} ({holder}) answers \
                 this session mail=skipped: the same"
            ));
            return Ok(());
        }
        if self.withhold_badge(seq, "ask", review)? {
            return Ok(());
        }
        let owner = format!("owner={ATTENTION_OWNER}");
        let r = self.call(&["meta", "set", "attention", &owner, &text])?;
        if r.ok() {
            self.attention_ours = Some(text);
            self.turn_end_badge = true;
        }
        review.note(&format!(
            "ESCALATED seq={seq} attention={} mail={MAIL_UNPARKED}",
            reply_word(&r)
        ));
        Ok(())
    }

    /// The in-GUI host's loop: `Self::watch`'s, under `opts` (normally
    /// [`SuperviseOpts::hosted`]), until `stop` is set. The flag is checked at
    /// every look and before every wait ([`Self::call`] refuses a wait or a
    /// write once it is set), so the loop ends within one wait: 20 s at most
    /// ([`WAIT_STEP`]), a ride-out pause 8 s at most. [`Ctl::interrupter`]
    /// ends a wait at once but loses the connection, so the badges could no
    /// longer be cleared. Every line goes to `out` and, with `opts.journal`,
    /// the journal; the approval ledger is kept at `opts.ledger` unless one
    /// was set. The badges it raised are cleared as it ends. `Ok(())` for a
    /// stop or a spent budget, `Err` with the reason for anything else.
    pub fn run_hosted(
        &mut self,
        opts: &SuperviseOpts,
        stop: Arc<AtomicBool>,
        out: &mut (dyn Write + Send),
    ) -> Result<(), String> {
        self.stop = Some(stop);
        // No teller rides beside the hosted loop: the story lines it owes the
        // window (`story chose …`) go out on its own connection.
        self.tell_own = true;
        if self.ledger_path.is_none() {
            self.ledger_path.clone_from(&opts.ledger);
        }
        let allow = opts.allow();
        let mut warn = std::io::stderr();
        if let Some(path) = opts.journal.as_deref() {
            approvals::bound(path);
        }
        let journal = Journal::open(opts.journal.as_deref(), self.sid.as_deref(), &mut warn);
        let sink = Mutex::new(Sink {
            out,
            journal,
            warn: Some(&mut warn),
            teller: None,
        });
        let mut review = Lines {
            sink: &sink,
            allow: &allow,
        };
        let end = self.run_loop(opts, &sink, None::<&mut NoLane>, &mut review);
        // A stop that cut a request short (the transport's interrupter)
        // ends the loop as a stop, not as the failure the cut request saw.
        let end = if self.stopped() { Ok(End::Quit) } else { end };
        let handed_over = self.stopped()
            && self
                .handover
                .as_ref()
                .is_some_and(|h| h.load(Ordering::SeqCst));
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
            .line(&line, None);
        result
    }
}
