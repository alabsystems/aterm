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
//! * `ERR busy …` and `ERR rate` back the loop off ([`Session::back_off`]):
//!   it waits for the refused point to leave the screen, bounded by a pause
//!   that doubles from 250 ms to 8 s.
//! * At the start ([`Session::take_claim`]) the claim is taken, and
//!   ([`Session::reconcile`]) a keyed attention entry a previous supervisor
//!   left is adopted when the screen still shows its point, else unset. A loop
//!   behind another supervisor's claim settles nothing.
//! * At the end ([`Session::release_attention`]) this loop's badges are
//!   cleared, if still its own, and the claim is released.

use super::escalate::{point_label, status_field};
use super::*;

impl<C: Ctl> Session<'_, C> {
    /// The hosted loop's stop flag is set.
    pub(super) fn stopped(&self) -> bool {
        self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst))
    }

    /// Record the session's working directory from a bare `meta` reply
    /// (`cwd=<pct>`; `-` is unknown).
    pub(super) fn note_cwd(&mut self, meta: &str) {
        let Some(line) = meta.lines().find(|l| l.starts_with("OK")) else {
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

    /// `ERR halted`: wait for the hold to lift. `status hold=1` is waited
    /// out with `await inbox since=0 kinds=hold` (the hold's transition
    /// latches it), a step at most, then read again; `hold=0` returns. A
    /// host whose `status` names no hold, or that refuses the wait, gets one
    /// step (`await seq`) and the loop looks again — the next press says
    /// whether the halt stands. Journaled `HALTED seq=<n> <why>` and
    /// `UNHALTED seq=<n> after <ms> ms`.
    pub(super) fn park_on_hold(
        &mut self,
        why: &str,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        review.note(&format!("HALTED seq={seq} {}", clip(&one_line(why))));
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

    /// `ERR busy …` / `ERR rate`: wait [`Session::refusal_pause`] (doubling
    /// to [`REFUSAL_PAUSE_MAX`]) or until `anchor` leaves the screen, as the
    /// server's own wait — never a sleep here — then the loop looks again.
    /// Journaled `REFUSED seq=<n> backoff=<ms> <why>`.
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
        review.note(&format!(
            "REFUSED seq={seq} backoff={} {}",
            pause.as_millis(),
            clip(&one_line(why))
        ));
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
    /// attention=<reply>`. The `meta` read also gives the session's cwd
    /// ([`Self::note_cwd`]).
    pub(super) fn reconcile(&mut self, review: &mut dyn Review) -> Result<(), Fail> {
        let read = self.call(&["meta"])?;
        if self.unserved(&read) {
            return Err(Fail::Lost(format!("meta failed: {}", read.stderr.trim())));
        }
        if !read.ok() || self.claim.watching_behind().is_some() {
            return Ok(());
        }
        self.note_cwd(&read.stdout);
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
                    "CLEARED seq=- hidden attention={} (an entry of ours may sit under \
                     another owner's)",
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
            review.note(&format!("ADOPTED seq={seq} attention={}", clip(&text)));
        } else {
            let owner = format!("owner={ATTENTION_OWNER}");
            let r = self.call(&["meta", "unset", "attention", &owner])?;
            review.note(&format!(
                "CLEARED seq={seq} stale attention={} (its point is not on the screen)",
                reply_word(&r)
            ));
        }
        Ok(())
    }

    /// A watch's last act: the badges it raised — a point's, a limit's — are
    /// cleared if they are still its own; a session already gone answers
    /// nothing and nothing is said.
    pub(super) fn release_attention(&mut self, review: &mut dyn Review) {
        let seq = self.last.as_ref().map_or(0, |s| s.seq);
        if self.box_ask.is_some() {
            let _ = self.close_box(seq, review);
        }
        if self.limit.is_some() {
            let _ = self.close_episode(seq, "the watcher ended", review);
        }
        if std::mem::take(&mut self.turn_end_badge) {
            let _ = self.unset_if_ours(|_| true);
        }
        self.release_claim();
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
