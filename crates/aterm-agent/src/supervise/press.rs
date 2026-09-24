// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop's two keystrokes on a box: the approving `1`, under the guard the
//! approval policy returned ([`Session::press_one_guarded`]), and
//! `--dismiss-surveys`' `0` ([`Session::dismiss_survey`]).
//!
//! **What binds the `1` to the box that was judged.** The guard is the judged
//! row itself, anchored ([`super::super::policy::row_guard`]): the server
//! tests it against every visible row under the same terminal lock as the
//! write, so a box swapped between the read and the press — another command,
//! another path — matches nothing and the answer is `OK skipped` (audit
//! APR-6 measured the old guard, `Do.you.want.to.proceed`, approving a
//! swapped rm box). Where the server fences a press on the screen generation
//! (`key if-gen=<epoch>.<seq>`, read from `help key` once) and the judged read
//! carried one (`text --json`'s `"gen"`, [`Screen::generation`]), that
//! generation is sent too, and `OK skipped reason=changed` means the screen
//! moved: the loop reads and decides again. A screen that keeps moving under
//! the press (a blinking row, 2.1.281's auto-deny countdown) is handed to
//! the manager after [`MAX_CHANGED_PRESSES`] such skips — never pressed
//! with the fence dropped: the row guard alone names the box's first command
//! row, which a swapped box can show too. The content sequence is no fence
//! (`if-seq=`, the spelling this press used before lane C's review): it is
//! per grid and repeats after an alternate-screen re-entry, and the server
//! refuses it.
//!
//! **What the server can say instead.** `ERR halted …` ([`Pressing::Halted`])
//! parks the loop until the hold lifts; `ERR busy …` and `ERR rate`
//! ([`Pressing::Refused`], `busy sink` among them) back it off; in both the
//! box is read and decided again after, never pressed again as it was.

use super::*;

impl<C: Ctl> Session<'_, C> {
    /// Whether the server fences a press on the screen generation: `help
    /// key` asked once (and again after an outage forgot the caps).
    pub(super) fn gen_fence_known(&mut self) -> Result<bool, Fail> {
        if let Some(known) = self.caps.gen_fence {
            return Ok(known);
        }
        let r = self.call(&["help", "key"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("help key failed: {}", r.stderr.trim())));
        }
        let known = r.ok() && super::super::policy::server_fences_gen(&r.stdout);
        self.caps.gen_fence = Some(known);
        Ok(known)
    }

    /// Press `1` on the box `prompt` read at `seen`, under `guard` — the
    /// check and the press under one server lock where the server has `key
    /// if=` (`OK skipped seq=<n>`: no row matched, nothing written) — else
    /// read → confirm the same box is still up and the guarded row still
    /// drawn → press → wait for the worker to take it → re-read → backspace
    /// a digit that landed in the composer. The fallback presses nothing
    /// while the session survey is open on the confirming read
    /// ([`Pressing::Withheld`]): a `1` that reaches the survey instead of
    /// the box is a rating, and no digit is left to find.
    ///
    /// The capability probe is the reply: `OK …` proves the guard (pressed
    /// or skipped); a usage line or a bare `ERR` — what a build without
    /// `if=` answers, reading `if=…` as a key name — drops the generation
    /// fence first, then falls back for good; `halted`, `busy …` and `rate`
    /// are [`Pressing::Halted`] and [`Pressing::Refused`]; every other `ERR`
    /// (`no such session`, `badregex`) is an error, never an approval.
    ///
    /// A lost connection is [`Pressing::Lost`]: a press whose answer never
    /// came is not an approval, and is not sent again as it was — the loop
    /// rides the connection out, then reads and decides again
    /// ([`Self::drive`]). On the fallback path a `1` the server confirmed is
    /// written, lost connection or not: it is the approval it was, and what
    /// could not be checked after it — a digit in the composer — is checked
    /// on the next turn ([`Self::stray`]).
    pub(super) fn press_one_guarded(
        &mut self,
        prompt: &Prompt,
        guard: &str,
        digit: &str,
        seen: &Screen,
    ) -> Result<Pressing, Fail> {
        if self.caps.key_if != Some(false) {
            // The fence needs both halves: a host that names it (`help key`,
            // asked once) and a read that carried the generation it names.
            let mut fence = if self.gen_fence_known()? {
                seen.generation.clone()
            } else {
                None
            };
            loop {
                let args = super::super::policy::key_args(guard, digit, fence.as_deref());
                let mut words: Vec<&str> = vec!["key"];
                words.extend(args.split(' '));
                let r = self.call(&words)?;
                if r.ok() {
                    self.caps.key_if = Some(true);
                    let seq = r.seq().unwrap_or(seen.seq);
                    return Ok(Pressing::Done(if r.skipped_changed() {
                        Press::Changed { seq }
                    } else if r.skipped() {
                        Press::Skipped { seq }
                    } else {
                        Press::Pressed { seq }
                    }));
                }
                let why = format!("key {args} failed: {}", r.stderr.trim());
                if self.unserved(&r) {
                    // The guard ran under the server's lock or not at all: a
                    // `1` it wrote went to the box, never the composer.
                    return Ok(Pressing::Lost { known: None, why });
                }
                if r.is_err("halted") {
                    return Ok(Pressing::Halted { why });
                }
                if r.is_err("busy") || r.is_err("rate") {
                    return Ok(Pressing::Refused { why });
                }
                if r.unknown_form() {
                    if fence.take().is_some() {
                        // The fence is what it did not know: the row guard
                        // alone, from now on.
                        self.caps.gen_fence = Some(false);
                        continue;
                    }
                    self.caps.key_if = Some(false);
                    break;
                }
                return Err(Fail::Hard(why));
            }
        }
        let now = self.screen()?;
        let still = parse_prompt(&now.rows).is_some_and(|q| q.command == prompt.command)
            && aterm_observe::row_matcher(guard)
                .is_ok_and(|m| now.rows.iter().any(|r| m.matches(r)));
        if !still {
            return Ok(Pressing::Done(Press::Skipped { seq: now.seq }));
        }
        // An unguarded `1` goes wherever keys go when it lands: to the box,
        // or — the box resolved first — to the session survey parked above
        // the composer, where it is the rating `Bad`, the human's to give, and
        // leaves no digit in the composer to find. With the survey open,
        // nothing is pressed: the box is the manager's.
        if survey_open(&now.rows) {
            return Ok(Pressing::Withheld);
        }
        let r = self.call(&["key", digit])?;
        if !r.ok() {
            let why = format!("key {digit} failed: {}", r.stderr.trim());
            if self.unserved(&r) {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost { known: None, why });
            }
            if r.is_err("halted") {
                return Ok(Pressing::Halted { why });
            }
            if r.is_err("busy") || r.is_err("rate") {
                return Ok(Pressing::Refused { why });
            }
            return Err(Fail::Hard(why));
        }
        let seq = r.seq().unwrap_or(now.seq);
        let pressed = Press::Pressed { seq };
        // Let the worker take the press before looking for a stray digit: the
        // first content change after it, bounded.
        let after = match self
            .wait(&["seq", &seq.to_string()], STRAY_SETTLE)
            .and_then(|_| self.screen())
        {
            Ok(after) => after,
            Err(Fail::Lost(why)) => {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost {
                    known: Some(pressed),
                    why,
                });
            }
            Err(e) => return Err(e),
        };
        if stray_digit(&after, digit) {
            let r = self.call(&["key", "backspace"])?;
            let skipped = Press::Skipped { seq: after.seq };
            if self.unserved(&r) {
                self.stray = Some(prompt.command.clone());
                return Ok(Pressing::Lost {
                    known: Some(skipped),
                    why: format!("key backspace failed: {}", r.stderr.trim()),
                });
            }
            return Ok(Pressing::Done(skipped));
        }
        Ok(Pressing::Done(pressed))
    }

    /// The choice on an UNNUMBERED dialog (the folder-trust dialog,
    /// [`Choice::Focus`](super::super::policy::approval::Choice::Focus)):
    /// move the focus `steps` options — each arrow fenced on the generation
    /// of the read that showed the dialog as judged (`key if-gen=<g>
    /// if=<the judged row> down`), the screen read again once it has moved
    /// and held still (`await seq`, then `await idle`) — then,
    /// once a fresh read by the session's reader shows the SAME dialog
    /// (kind, folder) with the focus on the option labelled `label`, press
    /// Enter fenced on THAT read's generation and guarded on the focused row
    /// itself, `❯` and all: the Enter lands only while the focus is where
    /// the read saw it. Never without the generation fence (a host or a read
    /// without it: [`Pressing::Unconfirmed`]), never by the fallback's
    /// unguarded keys, and never Esc (the trust dialog's cancel EXITS Claude
    /// Code). A focus that did not land where it was sent, or a dialog that
    /// changed under the moves, is [`Pressing::Unconfirmed`]: the box is the
    /// manager's. More than [`MAX_FOCUS_STEPS`] moves is too.
    pub(super) fn press_focused(
        &mut self,
        prompt: &Prompt,
        steps: i32,
        label: &str,
        guard: &str,
        seen: &Screen,
    ) -> Result<Pressing, Fail> {
        let unconfirmed = |why: &str| {
            Ok(Pressing::Unconfirmed {
                why: why.to_string(),
            })
        };
        if steps.unsigned_abs() > MAX_FOCUS_STEPS {
            return unconfirmed("the focus is too far from the option to move it");
        }
        if self.caps.key_if == Some(false) || !self.gen_fence_known()? {
            return unconfirmed("this host cannot fence a focus move on the screen generation");
        }
        let arrow = if steps >= 0 { "down" } else { "up" };
        let program = self.program.clone();
        let focused_on = move |rows: &[String]| {
            aterm_phase::read(program.as_deref(), rows, None)
                .prompt
                .and_then(|q| q.focused().map(|o| o.label.clone()))
        };
        let before = focused_on(&seen.rows);
        let mut now = seen.clone();
        for _ in 0..steps.unsigned_abs() {
            let Some(generation) = now.generation.clone() else {
                return unconfirmed("the read carried no screen generation to fence on");
            };
            let args = super::super::policy::key_args(guard, arrow, Some(&generation));
            let mut words: Vec<&str> = vec!["key"];
            words.extend(args.split(' '));
            let r = self.call(&words)?;
            if let Some(p) = self.refused_press(&r, &args)? {
                return Ok(p);
            }
            let at = r.seq().unwrap_or(now.seq);
            if r.skipped_changed() {
                return Ok(Pressing::Done(Press::Changed { seq: at }));
            }
            if r.skipped() {
                return Ok(Pressing::Done(Press::Skipped { seq: at }));
            }
            // The move drawn and SETTLED, then read — a redraw arrives in
            // several writes (clear, then rows), and a read at the first
            // change saw a dialog half drawn and no focus (measured on this
            // lane's live check). Both waits bounded, never a sleep.
            self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
        let reading = aterm_phase::read(self.program.as_deref(), &now.rows, None);
        let focused = reading.prompt.as_ref().and_then(|q| {
            (q.kind == prompt.kind && q.command == prompt.command)
                .then(|| q.focused())
                .flatten()
        });
        let Some(focused) = focused.filter(|o| o.label == label) else {
            // The same dialog, the focus where it was: the keys went nowhere
            // — a dialog drawn before its input is live takes none (measured
            // live 2026-09-24: an arrow 0.3 s after Claude Code 2.1.281 drew
            // the trust dialog was dropped). Read and decided again, like a
            // screen that moved; a focus that moved ELSEWHERE is the
            // manager's.
            if before.is_some() && focused_on(&now.rows) == before {
                return Ok(Pressing::Done(Press::Changed { seq: now.seq }));
            }
            return unconfirmed("the focus did not land on the option it was moved to");
        };
        let Some(generation) = now.generation.clone() else {
            return unconfirmed("the read carried no screen generation to fence on");
        };
        let enter_guard = super::super::policy::row_guard(&now.rows[focused.row]);
        let args = super::super::policy::key_args(&enter_guard, "enter", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        if let Some(p) = self.refused_press(&r, &args)? {
            return Ok(p);
        }
        let at = r.seq().unwrap_or(now.seq);
        Ok(Pressing::Done(if r.skipped_changed() {
            Press::Changed { seq: at }
        } else if r.skipped() {
            Press::Skipped { seq: at }
        } else {
            Press::Pressed { seq: at }
        }))
    }

    /// A fenced press the server did not take, as the loop's [`Pressing`]
    /// (`None`: it answered `OK …`, pressed or skipped): not served — lost;
    /// `halted`; `busy`/`rate`; anything else, a usage line included, is
    /// [`Pressing::Unconfirmed`] — the press this path makes is never
    /// retried without its fence.
    pub(super) fn refused_press(
        &mut self,
        r: &CtlReply,
        args: &str,
    ) -> Result<Option<Pressing>, Fail> {
        if r.ok() {
            self.caps.key_if = Some(true);
            return Ok(None);
        }
        let why = format!("key {args} failed: {}", r.stderr.trim());
        Ok(Some(if self.unserved(r) {
            Pressing::Lost { known: None, why }
        } else if r.is_err("halted") {
            Pressing::Halted { why }
        } else if r.is_err("busy") || r.is_err("rate") {
            Pressing::Refused { why }
        } else {
            Pressing::Unconfirmed { why }
        }))
    }

    /// `--dismiss-surveys`' press: `key if=^●.How.is.Claude.doing 0`, the
    /// check and the press under one server lock, as the approval press
    /// guards its `1` — `OK skipped seq=<n>` means no row matched and nothing
    /// was written, so a survey that left first never gets a `0` in the
    /// composer (a copy quoted in the transcript does not match:
    /// [`survey_row`]).
    /// `0` is the only key it ever presses. A host without the guard (a usage
    /// line or a bare `ERR`) is never pressed unguarded: [`Dismiss::Unguarded`],
    /// and the survey is said instead. `ERR busy sink` is retried
    /// [`BUSY_SINK_RETRIES`] times (the guard is the survey's own row, so a
    /// retry cannot land anywhere else); `ERR halted`, a busy sink that does
    /// not clear, `ERR busy …` and `ERR rate` are [`Dismiss::Refused`] — the
    /// loop parks or backs off and tries the survey again; a request not
    /// served is ridden out; any other `ERR` ends the loop. `seen` is the seq
    /// of the screen read before.
    pub(super) fn dismiss_survey(&mut self, seen: u64) -> Result<Dismiss, Fail> {
        if self.caps.key_if == Some(false) {
            return Ok(Dismiss::Unguarded);
        }
        let cond = format!("if={}", survey_row());
        let mut busy = 0;
        loop {
            let r = self.call(&["key", &cond, "0"])?;
            if r.ok() {
                self.caps.key_if = Some(true);
                return Ok(if r.skipped() {
                    Dismiss::Skipped
                } else {
                    Dismiss::Pressed {
                        seq: r.seq().unwrap_or(seen),
                    }
                });
            }
            let why = format!("key {cond} 0 failed: {}", r.stderr.trim());
            if self.unserved(&r) {
                return Err(Fail::Lost(why));
            }
            if r.unknown_form() {
                self.caps.key_if = Some(false);
                return Ok(Dismiss::Unguarded);
            }
            if r.is_err("busy sink") && busy < BUSY_SINK_RETRIES {
                busy += 1;
                std::thread::sleep(BUSY_SINK_BACKOFF);
                continue;
            }
            if r.is_err("halted") {
                return Ok(Dismiss::Refused { halted: true, why });
            }
            if r.is_err("busy") || r.is_err("rate") {
                return Ok(Dismiss::Refused { halted: false, why });
            }
            return Err(Fail::Hard(why));
        }
    }
}
