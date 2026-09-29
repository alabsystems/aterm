// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The turn-end policy in the loop: every point where a worker's turn ended
//! is decided by [`decide_turn_end`] — the ONE turn-end decider — and the
//! loop only carries out what it says.
//!
//! What the loop adds around the pure decision is what the decision cannot
//! know: the reading of the screen by the reader for the session's program
//! (`aterm-phase`), the work the turn did (the first busy read since the last
//! point, [`Session::running`]), a wall's reset on the loop's clock (read
//! once per notice text, so a span counts from its print), and the standing
//! rules (the `rules_file`, cut at [`RULES_CAP`]). Nothing
//! it types goes without:
//!
//! * the session's foreground program read FRESH (`status program=`), its
//!   reader a supervised agent's — Claude Code's or Codex's — and every read
//!   of the composer that reader's ([`Session::reader`]);
//! * THE FENCED WRITE ([`Session::type_fenced`]), where the host fences
//!   `send` on the screen generation: `send if-gen=<the judged read> if=<its
//!   composer row> -- <text>` writes the text only while the screen is still
//!   the one judged — a person's first keystroke since, a box, anything that
//!   moved it, and nothing is written (`OK skipped reason=changed`; the
//!   point is decided again shortly; a guard that matches no row of the
//!   UNCHANGED screen, a bare `OK skipped`, is escalated with what was not
//!   typed, since a retry would miss the same way) — then a read must show
//!   the composer
//!   holding exactly that text (however it wrapped) before Enter goes,
//!   fenced on THAT read (`key if-gen=… if=<the caret row> enter`) — for a
//!   program whose paste guard takes an Enter right behind typed text as a
//!   newline (Codex, measured: [`aterm_phase::ScreenReader::paste_guard`])
//!   once the echo has held still [`PASTE_GUARD_SETTLE_MS`]. Text we wrote
//!   and did not submit is escalated, never left silently;
//! * elsewhere, the guarded submit (`turn submit=guarded:<re> yield=0.2`,
//!   lane C): parked while a person types (`yield=`), then pasted, and Enter
//!   pressed only while the row holding the CURSOR — a row the composer
//!   draws, its caret row or a continuation indented two, never a shell's —
//!   still ends as the text does ([`composer_guard`]) — the PASTE is not
//!   fenced there, only checked against the read that judged the point, so a
//!   person who starts typing in between can get it spliced into a draft; a
//!   miss (`skipped`) leaves the text typed, and is escalated. A host without
//!   either gets nothing typed;
//! * for the worker's own suggestion, the vendor's accept key — `right` on
//!   an empty composer with a suggestion shown fills it (Claude Code 2.1.280,
//!   read from its binary: `right` and `tab` both `markAccepted` and fill the
//!   text; `right` is the one with no other use on an empty composer) —
//!   fenced on the judged read's generation and guarded on the suggestion's
//!   row, then Enter fenced on the read that shows it filled. A host without
//!   the generation fence gets the suggestion's words typed instead (the
//!   vendor counts a submitted text equal to its suggestion as accepted);
//! * for a draft left standing ([`TurnEndAction::Submit`]), Enter alone,
//!   fenced on the judged read's generation and guarded on the caret row as
//!   judged — a keystroke since and nothing is pressed. A host without the
//!   fence gets nothing pressed: the draft is escalated instead.
//!
//! A `WaitUntil` is the deadline of the loop's wait for the screen to move
//! ([`Session::wait_for_next`]): a change first decides again at the next
//! look, the deadline decides again on the point still showing
//! ([`Session::turn_end_now`]). Every act is a ledger row under its rule id
//! and a printed line — `CONTINUED seq=<n> rule=<id> <text>` for a
//! continuation or an accepted suggestion, `TYPED seq=<n> rule=<id>
//! <command>` for a slash command; a wait is journaled `WAITING seq=<n>
//! until=<UTC> <why>` once per deadline, an act not taken `SKIPPED
//! seq=<n> rule=<id> <why>`, and a task the worker said is done after the
//! done check `DONE seq=<n> rule=continue-done-check@v1 …`, once.

use super::super::codex_usage::{self, CodexSeen, LimitRead};
use super::super::policy::approval::RULE_RATE_NUDGE_SWITCH;
use super::super::policy::approval::squashed;
use super::super::policy::turn_end::{
    CodexSetting, GOAL_PAUSE, GOAL_RESUME, GoalStop, RULE_DONE_CHECK, RULE_LIMIT_RESUME,
    RULE_MODEL_RESTORE, RULE_WIND_DOWN, Restart, StoodBy, Then, WIND_DOWN_BOUND, WindDown,
    WindEvent, WindPhase, decide_turn_end, wind_phase_word,
};
use super::*;

/// How long one look at Codex's own records ([`Session::codex_records`]) is
/// taken as current: it reads a few hundred files' times and the ends of
/// the newest rollouts.
const CODEX_LOOK_EVERY: Duration = Duration::from_secs(60);

/// How long a save-then-wait switch may stand still — owed its save, winding
/// down, owed its model back — through one break under a Codex background
/// terminal before a person is told ([`Session::switch_at_background`]).
pub(super) const SWITCH_BREAK_NOTE: Duration = Duration::from_secs(10 * 60);

/// A break under a Codex background terminal the switch has taken as its
/// point ([`Session::switch_at_background`]).
#[derive(Debug, Clone)]
pub(super) struct SwitchBreak {
    /// Since when the switch has stood at `phase` in this break.
    pub(super) since: Instant,
    /// The switch's phase word then.
    pub(super) phase: &'static str,
    /// Its escalation raised already.
    pub(super) escalated: bool,
    /// The review key of the screen last decided at it: the same screen
    /// read again is decided again only at the wait its decision named.
    pub(super) key: String,
    /// That decision's reading showed a draft in the composer.
    pub(super) draft: bool,
    /// What of the policy's own held the switch at that decision
    /// ([`TurnEndState::switch_hold`]).
    pub(super) hold: Option<StoodBy>,
}

/// The longest text the cursor-row guard names whole, as `^❯ <text>`: it
/// sits on the caret row of any composer 43 columns wide or more.
const GUARD_TAIL_CHARS: usize = 40;

/// The most of the text's last word a longer text's guard names: the tail
/// of a word too long for the composer's width is what its last row holds.
const GUARD_WORD_CHARS: usize = 16;

/// `yield=<floor>` on every `turn` this policy types: the verb parks BEFORE
/// typing until the target's typing momentum has exhaled to the floor, so a
/// person typing is not pasted over (the floor the harness's own typed
/// turns use). A yield that outlives the turn's timeout types nothing
/// (`ERR yield timeout`).
const TURN_YIELD: &str = "yield=0.2";

/// The standing rules ride in the continuation cut to this many characters
/// (at a word, `…` after): a continuation stays a line a composer holds as
/// typed text, not a pasted-text placeholder the fence cannot read back.
pub(super) const RULES_CAP: usize = 400;

/// A fenced write the server skipped (the screen moved between the judged
/// read and the write — a person's keystroke, a box): the point is decided
/// again this far off, on the deadline of the loop's own wait.
const REDECIDE_AFTER_SKIP: Duration = Duration::from_secs(2);

/// The longest the loop waits for the worker to REACT to its own write —
/// the continuation's echo ([`Session::type_fenced`]), the accept key's fill
/// ([`Session::accept_suggestion`]) — before it reads the composer to judge
/// the write. The wait ends the moment the screen moves, so a worker that
/// reacts pays nothing for it; the bound only says when one that has not
/// reacted AT ALL is judged anyway. Judged earlier, the read sees the
/// composer as it was before the write: a continuation merely not drawn yet
/// was escalated as "typed, not submitted", and an accept key merely not
/// read yet was taken for one that fills nothing, its words typed after it —
/// the worker then read both, and the Enter fenced on the read that showed
/// the fill submitted them doubled (`keep goingkeep going`). Both measured
/// with the live headless tests' worker on a loaded machine (load 35-40),
/// where it went more than 2 s without reacting; this was the approval
/// press's 2 s settle, whose worker takes a single digit. What it does NOT
/// cover: a worker starved past this bound is judged on the old composer
/// as before, and one that starts to react and then stalls mid-draw is
/// read once the screen is still for [`SETTLE_MS`] or [`SETTLE_CAP`] has
/// passed — a composer read half drawn is escalated, as it should be.
pub(super) const REACTION_WAIT: Duration = Duration::from_secs(10);

/// How long the echo of a fenced write must hold still before its Enter, for
/// a program with a paste guard ([`ScreenReader::paste_guard`]: Codex takes
/// an Enter within a burst of typed text as a newline): the guarded `turn`'s
/// own settle (`idle=600`), under which Codex submitted every time
/// (measured on 0.156.1).
const PASTE_GUARD_SETTLE_MS: &str = "600";

/// The cursor-row guard for `text` typed into the composer whose caret row
/// starts with `caret` (the program's, [`ScreenReader::caret`]: `❯` Claude
/// Code, `›` Codex): `^<caret> <text>` to the row's end when the text is
/// short enough to sit on the caret row ([`GUARD_TAIL_CHARS`]); else the end
/// of its last WORD (at most [`GUARD_WORD_CHARS`]) to the row's end, on a
/// row the COMPOSER draws — its caret row, or a continuation row indented
/// two. Both wrap their composer between words, and where the wrap falls
/// depends on the width, so the row the cursor ends on holds the last word —
/// never a fixed count of characters (lane B2's review: a 40-character tail
/// missed whenever the wrap put fewer on the last row). Unanchored, that
/// tail matched a SHELL's cursor row too: the hazards review of 2026-09-25
/// fed `answer_text` to a zsh row and the guard passed, so an agent that
/// exited mid-write would have had its answer run as a command. A shell's
/// row opens with its prompt, and its soft-wrapped continuation with the
/// text itself — neither with a caret or two spaces. Whitespace-free
/// ([`super::super::policy::row_guard`]'s escaping).
pub(in crate::supervise) fn composer_guard(caret: char, text: &str) -> String {
    if text.chars().count() <= GUARD_TAIL_CHARS {
        // The caret's space is whatever the program draws there — a
        // NO-BREAK SPACE on Claude Code 2.1.280 and 2.1.281 (`❯\u{a0}keep
        // going`) — so it is `\s`, never the `\x20` a typed space is.
        let guard = super::super::policy::row_guard(&format!("{caret} {text}"));
        return guard.replacen(&format!("^{caret}\\x20"), &format!("^{caret}\\s"), 1);
    }
    let word = text.split_whitespace().last().unwrap_or(text);
    let n = word.chars().count();
    let tail: String = word
        .chars()
        .skip(n.saturating_sub(GUARD_WORD_CHARS))
        .collect();
    let anchored = super::super::policy::row_guard(&tail);
    // `row_guard` anchors at the row's start: the tail ends the row, on a
    // row that opens with the caret or the composer's two-space indent.
    format!("^[{caret}\\s]\\s.*{}", anchored.trim_start_matches('^'))
}

/// The composer's caret row as `reader` reads it (`None`: no composer).
fn caret_row(reader: &dyn ScreenReader, rows: &[String]) -> Option<usize> {
    reader.composer(rows).map(|(caret, _)| caret)
}

/// The text on the composer's caret row as `reader` reads it, the caret
/// stripped — typed, or the placeholder ([`typed_draft`] tells which).
fn caret_text(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    reader
        .composer(rows)
        .and_then(|(_, lines)| lines.into_iter().next())
}

/// A wall the host's measure of the API's reach answers: an API error
/// that never reached the API, a reply the connection cut off, or a
/// certificate or proxy refused ([`aterm_phase::ApiCause`]) — the arms of
/// the turn-end policy that read [`TurnEndReading::reach`]. The server's
/// own failure (a status, an overload) is not: a handshake says nothing
/// of the API's health, and it keeps its ladder whatever the measure.
fn answered_by_the_network(kind: aterm_phase::WallKind) -> bool {
    matches!(
        kind,
        aterm_phase::WallKind::ApiError {
            cause: aterm_phase::ApiCause::Unreachable
                | aterm_phase::ApiCause::CutOff
                | aterm_phase::ApiCause::Config,
            ..
        }
    )
}

/// `rules` as one line of at most [`RULES_CAP`] characters: cut at the last
/// word that fits, `…` after.
pub(super) fn capped_rules(line: &str) -> String {
    if line.chars().count() <= RULES_CAP {
        return line.to_string();
    }
    let head: String = line.chars().take(RULES_CAP).collect();
    let cut = head.rfind(' ').filter(|&i| i > 0).unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

impl<C: Ctl> Session<'_, C> {
    /// `t` on the loop's clock as unix seconds.
    fn unix_of(&self, t: Instant) -> i64 {
        let now = Instant::now();
        let secs = |d: Duration| i64::try_from((d.as_millis() + 500) / 1000).unwrap_or(0);
        if t >= now {
            self.now_unix() + secs(t - now)
        } else {
            self.now_unix() - secs(now - t)
        }
    }

    /// `t` on the loop's clock as a UTC stamp, for the journal.
    fn stamp_in(&self, t: Instant) -> String {
        let ahead = (t.saturating_duration_since(Instant::now()).as_millis() + 500) / 1000;
        let secs = self.now_unix() + i64::try_from(ahead).unwrap_or(0);
        utc_stamp(u64::try_from(secs).unwrap_or(0))
    }

    /// A Codex screen's footer, kept: the model and effort, and the goal —
    /// a box covers the footer, and the rate-limit nudge is answered by what
    /// the footer showed before it.
    pub(super) fn note_codex_screen(&mut self, rows: &[String]) {
        if aterm_phase::codex::footer_status(rows).is_none() {
            return;
        }
        if let Some((model, effort)) = aterm_phase::codex::footer_model(rows) {
            let setting = CodexSetting { model, effort };
            // A press only intended has landed once the footer shows the
            // cheaper model.
            self.turn_end.footer_seen(&setting);
            self.codex_setting = Some(setting);
        }
        self.codex_goal = aterm_phase::codex::goal_state(rows);
    }

    /// WHAT CODEX'S OWN RECORDS SAY of this session ([`codex_usage::look`]):
    /// its usage window, and whether its thread fell into a sandbox its
    /// launch bypassed — under the `$CODEX_HOME` the session's foreground
    /// process (its Codex TUI) runs with, read through the kernel as the
    /// approval policy reads the worker ([`Self::worker_env`]). The thread
    /// that decides is the session's own where it is known
    /// ([`codex_usage::own_thread`]: the writer lock the TUI holds open,
    /// else the thread its argv resumes), else the rollouts written around
    /// the session's last work ([`Session::last_busy_at`]; a hold carried on
    /// from the ledger, the hold's start). One look is current for
    /// [`CODEX_LOOK_EVERY`]. A process that cannot be read, or names no home,
    /// reads unknown.
    pub(super) fn codex_records(&mut self) -> CodexSeen {
        if let Some(fixed) = &self.codex_fixed {
            return fixed.clone();
        }
        if let Some((at, seen)) = &self.codex_seen
            && at.elapsed() < CODEX_LOOK_EVERY
        {
            return seen.clone();
        }
        let worker = match self.approval_env.worker.clone() {
            WorkerSource::Session => self.worker_env(),
            WorkerSource::Fixed(worker) => worker,
        };
        // When the session last worked, on the wall clock.
        let now = Instant::now();
        let wall = std::time::SystemTime::now();
        let worked_at = self
            .last_busy_at
            .or_else(|| match self.turn_end.wind()?.phase {
                WindPhase::Holding { since } => Some(since),
                _ => None,
            });
        let active_at = worked_at.map_or(wall, |t| {
            wall.checked_sub(now.saturating_duration_since(t))
                .unwrap_or(wall)
        });
        let seen = match worker {
            Ok(w) => match codex_usage::codex_home(w.var("CODEX_HOME"), w.var("HOME")) {
                Some(home) => {
                    let argv = w.argv().to_vec();
                    let cwd = argv
                        .iter()
                        .position(|a| a == "-C" || a == "--cd")
                        .and_then(|i| argv.get(i + 1).cloned())
                        .or_else(|| self.cwd.as_ref().map(|c| c.display().to_string()));
                    let open = w.pid().and_then(codex_usage::open_files);
                    let thread = codex_usage::own_thread(&argv, open.as_deref(), &home);
                    codex_usage::look(
                        &home,
                        codex_usage::Whose {
                            cwd: cwd.as_deref(),
                            argv: &argv,
                            thread: thread.as_deref(),
                            active_at,
                        },
                        wall,
                    )
                }
                None => CodexSeen {
                    limits: LimitRead::Unknown("no Codex home named"),
                    ..CodexSeen::default()
                },
            },
            Err(_) => CodexSeen {
                limits: LimitRead::Unknown("the Codex process could not be read"),
                ..CodexSeen::default()
            },
        };
        self.codex_seen = Some((Instant::now(), seen.clone()));
        seen
    }

    /// A fixed reading of Codex's records in place of the look (a test's).
    #[cfg(test)]
    pub(crate) fn set_codex_records(&mut self, seen: CodexSeen) {
        self.codex_fixed = Some(seen);
    }

    /// `unix` (epoch seconds) on the loop's clock.
    fn instant_of(&self, unix: i64) -> Instant {
        let now = Instant::now();
        let delta = unix - self.now_unix();
        let d = Duration::from_secs(delta.unsigned_abs());
        if delta >= 0 {
            now + d
        } else {
            now.checked_sub(d).unwrap_or(now)
        }
    }

    /// The words a switch's rows carry ([`approvals::wind_switch_words`]):
    /// its reset, its hold's start, its last Esc (while that turn's point is
    /// still to come), its opening and its press as unix seconds on the
    /// loop's clock.
    pub(super) fn wind_words(&self, w: &WindDown, phase: &str) -> String {
        let since = match w.phase {
            WindPhase::Holding { since } => Some(self.unix_of(since)),
            _ => None,
        };
        approvals::wind_switch_words(
            w,
            phase,
            w.back_at.map(|at| self.unix_of(at)),
            since,
            w.stop_at.map(|at| self.unix_of(at)),
            w.opened_at.map(|at| self.unix_of(at)),
            w.pressed_at.map(|at| self.unix_of(at)),
        )
    }

    /// THE TAB'S GOAL RECORD ([`crate::harness::goal_hold`]) beside this
    /// loop's ledger — the one file the live upgrade's pause and this
    /// switch both keep their hold of Codex's goal in; `None` with no ledger.
    fn goal_hold_path(&self) -> Option<std::path::PathBuf> {
        self.ledger_path
            .as_deref()
            .map(crate::harness::goal_hold::path_beside)
    }

    /// Whether the LIVE UPGRADE holds Codex's goal paused for its move (its
    /// hold on the tab's goal record still owes the goal its resume): the
    /// switch then claims nothing of that goal — it types no `/goal pause`
    /// and presses no Esc of its own into the turn the pause lets finish
    /// (the owner's decision of 2026-09-28: no running tool call is ever cut
    /// off), and the goal stays the upgrade's to resume.
    pub(super) fn goal_held_by_upgrade(&self) -> bool {
        self.goal_hold_path()
            .is_some_and(|p| crate::harness::goal_hold::held_by_upgrade(&p))
    }

    /// Whether the paused-goal box on screen is the LIVE UPGRADE'S TO ANSWER
    /// ([`crate::harness::goal_hold::box_is_upgrades`]): its hold owes the
    /// resume, the Codex it relaunched for the move still leads its terminal
    /// ([`Session::codex_leads`], the kernel's word), and the box was not
    /// left to a person — never another Codex's box, a person's own `codex
    /// resume` in the tab (the goal-pause review of 2026-09-28).
    pub(super) fn goal_box_is_upgrades(&self) -> bool {
        let leads = self.codex_leads;
        self.goal_hold_path()
            .is_some_and(|p| crate::harness::goal_hold::box_is_upgrades(&p, leads))
    }

    /// THE LIVE UPGRADE'S RESUME, MADE BY THIS LOOP, and `why`: the paused
    /// goal's box the relaunched Codex opened with, answered with its resume
    /// under `goal-resume@v1` (the approval policy's `goal_resume_pick`:
    /// `moved`), or the save-then-wait switch's `/goal resume` at its reset
    /// over a pause of the upgrade's (`switch`) — written to the upgrade's
    /// hold on the tab's goal record as its step would write it (`Resuming`:
    /// made, its showing on the footer the upgrade's next look verifies), so
    /// it is never made twice.
    pub(super) fn goal_resume_pressed(&mut self, why: &str) {
        use crate::harness::goal_hold::{self, Owner, Stage};
        let Some(path) = self.goal_hold_path() else {
            return;
        };
        let Some(mut h) = goal_hold::read(&path).filter(|h| h.owner == Owner::Upgrade && h.owes())
        else {
            return;
        };
        h.stage = Stage::Resuming;
        h.resume_at = u64::try_from(self.now_unix()).unwrap_or(0);
        h.resumes = h.resumes.saturating_add(1);
        h.why = why.to_string();
        let _ = goal_hold::write(&path, &h);
    }

    /// THE SWITCH'S CLAIM KEPT IN STEP with its own state
    /// ([`crate::harness::goal_hold::sync_switch`]): while the switch holds
    /// Codex's goal paused ([`WindDown::goal_paused`]), its hold is on the
    /// tab's goal record — where the live upgrade reads it, and never pauses
    /// the goal over it — and it ends there as the switch lets the goal go.
    /// Nothing is written while nothing changed.
    pub(super) fn sync_goal_hold(&mut self) {
        let Some(path) = self.goal_hold_path() else {
            return;
        };
        let (paused, how) = self
            .turn_end
            .wind()
            .filter(|_| self.turn_end.switch_open())
            .map_or((false, crate::harness::goal_hold::How::Esc), |w| {
                (
                    w.goal_paused,
                    if w.pause_typed {
                        crate::harness::goal_hold::How::Typed
                    } else {
                        crate::harness::goal_hold::How::Esc
                    },
                )
            });
        let now = u64::try_from(self.now_unix()).unwrap_or(0);
        let _ = crate::harness::goal_hold::sync_switch(&path, paused, how, now);
    }

    /// THE SWITCH THE NUDGE'S PRESS WILL OPEN (the approval policy's
    /// `rate-nudge-switch@v1`, its option `label`), made BEFORE the key goes:
    /// from the model the footer last showed, back at the window's reset
    /// Codex's records name, with a one-time marker, stamped as pressed now
    /// ([`WindDown::pressed_at`]: the floor under the thread's rollout — the
    /// goal turn Codex runs under the box begins after it). Its words are the
    /// press's INTENT row, ledgered before the key ([`Self::nudge_intended`]),
    /// so a loop that dies between the key and its record still carries the
    /// switch on; it opens here only once the box has left
    /// ([`TurnEndState::open_switch`]). `None` when the footer was never
    /// read (the policy switches nothing then).
    pub(super) fn nudge_intent(&mut self, label: &str) -> Option<WindDown> {
        let from = self.nudge_from()?;
        let to = label
            .trim()
            .trim_start_matches("Switch to ")
            .trim()
            .to_string();
        let back_unix = match self.codex_records().limits {
            LimitRead::Near { back_at, .. } => back_at,
            _ => None,
        };
        let back_at = back_unix.map(|u| self.instant_of(u));
        let salt = u64::try_from(self.now_unix()).unwrap_or(0);
        let marker =
            crate::harness::upgrade::saved_marker(self.sid.as_deref().unwrap_or("-"), &to, salt);
        Some(WindDown {
            pressed_at: Some(Instant::now()),
            ..WindDown::opened(from, to, back_at, marker)
        })
    }

    /// The model the rate-limit nudge is answered from: the footer's as last
    /// read, else — a loop that started at the box, which covers the footer
    /// — the `from` of a press only intended that it carried on
    /// ([`WindDown::intent`]).
    pub(super) fn nudge_from(&self) -> Option<CodexSetting> {
        self.codex_setting.clone().or_else(|| {
            self.turn_end
                .wind()
                .filter(|w| w.intent)
                .map(|w| w.from.clone())
        })
    }

    /// The press's INTENT, ledgered before its key (`phase=intent`): a row
    /// [`approvals::open_wind_down`] seeds from — as only intended
    /// ([`WindDown::intent`]), so a loop restarted before the key went
    /// decides the nudge still up again, and one restarted after it sees the
    /// switch land on the footer.
    pub(super) fn nudge_intended(&mut self, w: &WindDown, what: &str, seq: u64) {
        let words = self.wind_words(w, "intent");
        self.ledger_row(
            RULE_RATE_NUDGE_SWITCH,
            approvals::Outcome::Skipped,
            what,
            &format!("pressing; the switch opens once the box leaves {words}"),
            seq,
        );
    }

    /// The nudge's press DID NOT LAND (the box did not change, the key was
    /// never sent): the intent is closed (`phase=released`), and nothing of
    /// the switch goes on.
    pub(super) fn nudge_missed(&mut self, w: &WindDown, what: &str, why: &str, seq: u64) {
        let words = self.wind_words(w, "released");
        self.ledger_row(
            RULE_RATE_NUDGE_SWITCH,
            approvals::Outcome::Skipped,
            what,
            &format!("{why}: the switch is not open {words}"),
            seq,
        );
    }

    /// Whether the turn RUNNING NOW is a person's
    /// ([`RunningTurn::person`]): a keystroke of theirs, or a draft that
    /// changed, since the open switch opened ([`WindDown::opened_at`]) and
    /// within the turn's own work so far ([`Session::running`]: from its
    /// first busy read after the last point, a break, or the nudge's box) —
    /// or, for the turn in flight when this loop carried a switch on, since
    /// its last row and within [`SEEDED_TURN_BOUND`] of the loop's first read
    /// ([`WindDown::seeded_at`]) — latched until the turn's point; or one
    /// within `human_grace_s`, which holds the stop while it lasts.
    ///
    /// [`RunningTurn::person`]: super::super::policy::turn_end::RunningTurn::person
    /// [`SEEDED_TURN_BOUND`]: super::super::policy::turn_end::SEEDED_TURN_BOUND
    pub(super) fn person_in_this_turn(&mut self, now: Instant) -> bool {
        let (floor, seeded) = self
            .turn_end
            .wind()
            .map_or((None, None), |w| (w.opened_at, w.seeded_at));
        let typed = self.person_at();
        self.running
            .person(typed, floor, seeded, self.human_grace, now)
    }

    /// THE NUDGE'S BOX ANSWERED (its switch pressed, its keep): the turn it
    /// covered ended under it, so the turn running now — Codex's goal turn,
    /// started under the box within milliseconds — is measured from its own
    /// first busy read, and a person's hand latched before is none of it
    /// ([`RunningTurn::boxed`]).
    ///
    /// [`RunningTurn::boxed`]: super::super::policy::turn_end::RunningTurn::boxed
    pub(super) fn nudge_answered(&mut self) {
        self.running.boxed();
    }

    /// THE SAVE-THEN-WAIT SWITCH OPENS (the nudge's press seen landing, or
    /// carried as only intended): `wind` stamped with its opening — the floor
    /// under every person's keystroke the switch reads, and under the
    /// thread's rollout — after the box's span is closed
    /// ([`Self::nudge_answered`]).
    pub(super) fn open_wind(&mut self, wind: WindDown) {
        let now = Instant::now();
        self.nudge_answered();
        self.turn_end.open_switch(WindDown {
            opened_at: wind.opened_at.or(Some(now)),
            ..wind
        });
    }

    /// CODEX'S TURN STOPPED WHILE THE SWITCH IS OPEN (the owner: nothing goes
    /// on on the cheaper model; [`TurnEndState::goal_stop`]): read at the
    /// press's landing and at every busy read of an unattended loop. A turn
    /// running while the wind-down is owed or the model owed back — the goal
    /// turn Codex starts under the nudge's box within milliseconds of the
    /// turn's end (measured), one its goal starts again after a stop — or a
    /// goal turn during the hold, that is NO PERSON'S — no keystroke of
    /// theirs since the switch opened within the grace or within the turn's
    /// own work so far (never the turn the nudge's box covered: a person's
    /// message that began THAT turn spares no goal turn after it), and no
    /// busy read of it that saw their hand ([`Self::person_in_this_turn`]:
    /// their message, their `/goal resume`, however long it runs), read fresh
    /// from `status` before the key — gets ONE Esc guarded on its status row
    /// (the reader's busy guard): Esc stops the turn and pauses the goal in
    /// the same instant (measured 2026-09-28). Up to [`GOAL_STOPS`] stops a
    /// switch; past them the goal's next turn is said to a person, once. The
    /// save's own turn gets its ONE Esc once its busy work since the save was
    /// typed — the loop's span of it ([`Session::running`]) and the save's
    /// turns carried on past a wall before it — reaches [`WIND_DOWN_BOUND`]
    /// (the owner's rule: the cheaper model only commits and pushes); a save
    /// still running after it is said, once. A status row no longer up sends
    /// nothing. Journaled `INTERRUPTED`, ledgered under `model-wind-down@v1`.
    ///
    /// [`GOAL_STOPS`]: super::super::policy::turn_end::GOAL_STOPS
    pub(super) fn stop_codex_turn(
        &mut self,
        screen: &Screen,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if !self.turn_end.switch_open() {
            return Ok(());
        }
        let reader = self.reader(&screen.rows);
        if reader.program() != aterm_phase::Program::Codex {
            return Ok(());
        }
        // The live upgrade holds Codex's goal paused for its move: a GOAL'S
        // turn running is the one its pause lets finish, never stopped. The
        // switch's own save turn (winding down) is not: its bound stands
        // (the goal-pause review of 2026-09-28: skipped whole, the cheaper
        // model's save ran with no limit under the upgrade's hold).
        let winding = self
            .turn_end
            .wind()
            .is_some_and(|w| w.phase == WindPhase::Winding);
        if self.goal_held_by_upgrade() && !winding {
            return Ok(());
        }
        let guard = reader
            .busy_guard()
            .filter(|g| busy_up(g, &screen.rows) && reader.phase(&screen.rows) == Phase::Busy);
        self.note_codex_screen(&screen.rows);
        let now = Instant::now();
        let goal = self.codex_goal;
        // The running turn's busy work so far, as the loop's span keeps it.
        let busy = guard.as_ref().map(|_| {
            self.running
                .since()
                .map_or(Duration::ZERO, |s| now.saturating_duration_since(s))
        });
        let person = self.person_in_this_turn(now);
        let mut stop = self.turn_end.goal_stop(busy, goal, person, now);
        if stop.is_some() {
            // A person's hand read fresh before anything goes.
            self.program = self.foreground_program()?;
            let person = self.person_in_this_turn(now);
            stop = self.turn_end.goal_stop(busy, goal, person, now);
        }
        let seq = screen.seq;
        match (stop, guard) {
            (Some(GoalStop::Tell), _) => {
                self.turn_end.goal_told();
                let turn = Turn {
                    phase: Phase::Busy,
                    screen: screen.clone(),
                    timed_out: false,
                };
                self.wind_said(&turn, review)
            }
            (Some(GoalStop::Esc), Some(guard)) => {
                let args = super::super::policy::key_args(&guard, "esc", None);
                let mut words: Vec<&str> = vec!["key"];
                words.extend(args.split(' '));
                let r = self.call(&words)?;
                if self.unserved(&r) {
                    return Err(Fail::Lost(format!("key esc failed: {}", r.stderr.trim())));
                }
                if !r.ok() || r.skipped() {
                    review.note(&format!(
                        "SKIPPED seq={seq} rule={RULE_WIND_DOWN} no Codex turn running to stop"
                    ));
                    return Ok(());
                }
                self.turn_end.own_esc(Instant::now());
                self.sync_goal_hold();
                if winding {
                    review.note(&format!(
                        "INTERRUPTED seq={seq} rule={RULE_WIND_DOWN} the save's turn, past {} \
                         min of work: the cheaper model only commits and pushes",
                        WIND_DOWN_BOUND.as_secs() / 60
                    ));
                } else {
                    review.note(&format!(
                        "INTERRUPTED seq={seq} rule={RULE_WIND_DOWN} Codex's turn, so no work \
                         goes on while the session waits on its own model"
                    ));
                }
                let reason = self.wind_events_typed().map_or_else(
                    || "the turn-end policy".to_string(),
                    |w| format!("the turn-end policy {w}"),
                );
                self.ledger_row(
                    RULE_WIND_DOWN,
                    approvals::Outcome::Typed,
                    "esc",
                    &reason,
                    seq,
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The switch's events since the last look: the words of the last TYPED
    /// act's switch (for that act's own ledger row); every other event kept
    /// for [`Self::wind_said`].
    fn wind_events_typed(&mut self) -> Option<String> {
        let mut words = None;
        for e in self.turn_end.take_wind_events() {
            match e {
                WindEvent::Typed { wind, phase } => {
                    words = Some(self.wind_words(&wind, phase));
                }
                other => self.wind_backlog.push(other),
            }
        }
        words
    }

    /// The switch's edges ledgered (`skipped`, carrying the switch), its
    /// notes said, once each, as the point's escalation ([`escalate`]'s
    /// `ask`, the badge standing until the worker works) — the note that
    /// Codex's goal runs on after every stop KEPT UP while the switch stands
    /// and the goal runs ([`Session::goal_badge`]), and recorded for a person
    /// to see as the hold is — and the hold's start recorded for a person to
    /// see ([`Self::hold_said`]).
    pub(super) fn wind_said(&mut self, point: &Turn, review: &mut dyn Review) -> Result<(), Fail> {
        let _ = self.wind_events_typed();
        let seq = point.screen.seq;
        for e in std::mem::take(&mut self.wind_backlog) {
            match e {
                WindEvent::Edge {
                    rule,
                    what,
                    wind,
                    phase,
                } => {
                    let words = self.wind_words(&wind, phase);
                    review.note(&format!(
                        "SWITCH seq={seq} rule={rule} {what} (phase={phase})"
                    ));
                    self.ledger_row(
                        rule,
                        approvals::Outcome::Skipped,
                        &what,
                        &format!("the turn-end policy {words}"),
                        seq,
                    );
                }
                WindEvent::Note(text) => {
                    let attention =
                        escalate::attention_text(self.reader(&point.screen.rows), point, &text);
                    self.escalate(seq, &attention, None, "ask", review)?;
                    self.turn_end_badge = true;
                    self.note_row(&text, seq);
                }
                WindEvent::GoalNote(text) => {
                    let attention =
                        escalate::attention_text(self.reader(&point.screen.rows), point, &text);
                    self.escalate(seq, &attention, None, "ask", review)?;
                    self.goal_badge = true;
                    self.note_row(&text, seq);
                    if let Some(host) = &self.stall_host {
                        host.inform(&text);
                    }
                }
                WindEvent::Hold { wind } => self.hold_said(&wind, seq, review)?,
                WindEvent::Typed { .. } => {}
            }
        }
        self.sync_goal_hold();
        Ok(())
    }

    /// A NOTE OF THE SWITCH'S, SAID: its own `skipped` row, carrying the
    /// switch as it stands — its notes said among its words (`told=`) — so a
    /// restarted loop raises none of them again, even a note said where the
    /// switch then stands still and writes no other row (the stood-still
    /// note, the footer's). A switch the point closed has its closing row.
    fn note_row(&mut self, text: &str, seq: u64) {
        if let Some(w) = self.turn_end.wind().cloned() {
            let words = self.wind_words(&w, wind_phase_word(&w));
            self.ledger_row(
                RULE_WIND_DOWN,
                approvals::Outcome::Skipped,
                text,
                &format!("the turn-end policy {words}"),
                seq,
            );
        }
    }

    /// `unix` in the local zone, in a person's words: `Oct 4, 7:59 PM`.
    fn local_words(&mut self, unix: i64) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let local = u64::try_from(unix.saturating_add(self.local_offset_s())).unwrap_or(0);
        let stamp = aterm_types::rfc3339::format_rfc3339(local);
        let num = |a: usize, b: usize| stamp.get(a..b).and_then(|t| t.parse::<usize>().ok());
        match (num(5, 7), num(8, 10), num(11, 13), num(14, 16)) {
            (Some(month @ 1..=12), Some(day), Some(hour), Some(minute)) => {
                let (h12, half) = match hour {
                    0 => (12, "AM"),
                    1..=11 => (hour, "AM"),
                    12 => (12, "PM"),
                    _ => (hour - 12, "PM"),
                };
                format!("{} {day}, {h12}:{minute:02} {half}", MONTHS[month - 1])
            }
            _ => stamp,
        }
    }

    /// THE HOLD, MADE VISIBLE (it can last days — a weekly window at 90% or
    /// more): ONE record when it begins, in plain words — the model and
    /// effort it waits on and when its limit resets (or that nobody read the
    /// reset, and the latest it lasts) — said on the loop's journal
    /// (`HOLDING`), ledgered, and handed to the session's host as an
    /// information for a person (the window's Messages, no badge:
    /// [`crate::supervise::IdleHost::inform`]). The reading that ends the
    /// hold early is Codex's own usage record, which a held session no
    /// longer writes: an early reset is seen only when another Codex session
    /// on the account writes one, so the end named is the reset's.
    fn hold_said(&mut self, w: &WindDown, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        let model = w.from.words();
        let who = match w.saved {
            Some(true) => "Codex saved its work and waits",
            Some(false) => "Codex's save is unconfirmed; it waits",
            None => "Codex waits",
        };
        let text = match (w.back_at, w.phase) {
            (Some(at), _) => {
                let at = self.local_words(self.unix_of(at));
                format!("{who} for {model}'s limit to reset at {at}")
            }
            (None, WindPhase::Holding { since }) => {
                let end = since
                    .checked_add(self.turn_end.timing.unknown_hold)
                    .unwrap_or(since);
                let end = self.local_words(self.unix_of(end));
                format!("{who} on {model}; its reset is unread, so until {end} at the latest")
            }
            (None, _) => format!("{who} for {model}'s limit to reset"),
        };
        review.say(&format!(
            "HOLDING seq={seq} rule={RULE_MODEL_RESTORE} {text} (an earlier reset shows only \
             in another Codex session's usage record)"
        ))?;
        let words = self.wind_words(w, wind_phase_word(w));
        self.ledger_row(
            RULE_MODEL_RESTORE,
            approvals::Outcome::Skipped,
            &text,
            &format!("the turn-end policy {words}"),
            seq,
        );
        if let Some(host) = &self.stall_host {
            host.inform(&text);
        }
        Ok(())
    }

    /// A TURN'S END UNDER A CODEX BACKGROUND TERMINAL while the save-then-wait
    /// switch is open: Codex draws `1 background terminal running · /ps to
    /// view` under an ended turn, and the screen reads BUSY for as long as
    /// the terminal runs (a dev server: for ever) — so no point came, and the
    /// wind-down's end was never judged, `/model` never typed, the hold never
    /// begun nor ended: the thread sat on the cheaper model (the re-review of
    /// 2026-09-28). Here such a read — the screen, without the line, an
    /// ENDED TURN read authoritatively however it ended
    /// ([`aterm_phase::codex::ended_under_background`]: idle, a QUESTION —
    /// Codex often ends on an offer, and a rejected push on "Should I merge
    /// them and push again?" — or a wall or an API error) — is a POINT FOR
    /// THE SWITCH'S OWN STEPS and nothing else: the screen, its line
    /// blanked, is folded into the policy as the turn's end
    /// ([`TurnEndState::observe`], the turn's work taken once per break) and
    /// decided as an ordinary point is ([`decide_turn_end`], which gives an
    /// open switch every point: the marker judged, `/model`, the hold's
    /// clock, its resume; a wall's own rule where the switch leaves it one;
    /// the point's escalation raised once a break) — no host step (the
    /// loop's `host_steps_in_background` takes none while a switch is open),
    /// no report, no continuation, and nothing at all once the point has
    /// closed the switch. A wait it names is kept ([`Session::turn_end_due`])
    /// and the break decided again at it. A switch that has not moved past
    /// an owed save, a wind-down or a restore for [`SWITCH_BREAK_NOTE`] of
    /// one break — whatever holds it: the policy's own wait (the footer, a
    /// sandbox, its restores spent: [`TurnEndState::switch_hold`]), a
    /// person's typing, a draft, the terminal — is said to a person, once,
    /// naming what holds it; said on the same break read again before the
    /// wait its decision named, it is ONLY said — the break is not decided
    /// again, and the act and the wait that decision named stand (the
    /// round-4 re-review: a `/model` typed a second time into the restore).
    /// `true` when the read was taken as the switch's: it is no work of the
    /// worker's.
    pub(super) fn switch_at_background(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if !self.turn_end.switch_open()
            || !review.unattended()
            || self.claim.watching_behind().is_some()
        {
            self.switch_break = None;
            return Ok(false);
        }
        let reader = self.reader(&screen.rows);
        let ended_as = (reader.program() == aterm_phase::Program::Codex)
            .then(|| aterm_phase::codex::ended_under_background(&screen.rows))
            .flatten();
        let Some(ended_as) = ended_as else {
            self.switch_break = None;
            return Ok(false);
        };
        let now = Instant::now();
        // The turn's end: the background terminal's line blanked (the rows
        // keep their places, so the composer's row is where it is).
        let ended = Screen {
            rows: screen
                .rows
                .iter()
                .map(|r| {
                    if aterm_phase::codex::is_background_terminal_row(r) {
                        String::new()
                    } else {
                        r.clone()
                    }
                })
                .collect(),
            ..screen.clone()
        };
        let phase = self.turn_end.wind().map_or("-", |w| w.phase.word());
        let first = self.switch_break.is_none();
        let turn = Turn {
            phase: ended_as,
            screen: screen.clone(),
            timed_out: false,
        };
        let key = review_key(&turn, &[]);
        let bound = self.switch_break_note;
        let said = self
            .turn_end
            .wind()
            .is_some_and(|w| w.said.contains(&"background"));
        let due = self.turn_end_due.as_ref().map(|(t, _)| *t);
        let worked = match &mut self.switch_break {
            Some(b) => {
                if b.phase != phase {
                    b.phase = phase;
                    b.since = now;
                }
                // The same break read again decides nothing new before the
                // wait its last decision named — as the same point read
                // again at an ordinary turn end. A switch that has stood
                // still through it past the bound is said there, and only
                // said: the act and the wait that decision named stand.
                if b.key == key && due.is_none_or(|t| now < t) {
                    let stood = !matches!(phase, "holding" | "-") && now >= b.since + bound;
                    let (draft, hold) = (b.draft, b.hold);
                    if stood && !said {
                        self.stood_still_said(draft, hold, now, opts)?;
                        self.wind_said(&turn, review)?;
                    }
                    return Ok(true);
                }
                b.key = key;
                None
            }
            None => {
                self.switch_break = Some(SwitchBreak {
                    since: now,
                    phase,
                    escalated: false,
                    key,
                    draft: false,
                    hold: None,
                });
                // The turn ended here: its work, once.
                self.running.point(now)
            }
        };
        let mut r = self.turn_end_reading(&ended, worked, opts);
        self.turn_end_due = None;
        self.person_for_the_hold(&mut r, now)?;
        self.turn_end.observe(&r, now);
        let draft = r.composer == super::super::policy::turn_end::Composer::Typed;
        let hold = self.turn_end.switch_hold(&r);
        if let Some(b) = &mut self.switch_break {
            b.draft = draft;
            b.hold = hold;
        }
        // The point closed the switch (a person's `/model`, a switch that did
        // not land): the break is no longer the switch's, and nothing else is
        // done at it — its edges are ledgered.
        let action = if self.turn_end.switch_open() {
            self.decide_to_act(r, opts, now)?
        } else {
            TurnEndAction::Nothing
        };
        // A switch that has stood still through the break: said, once,
        // naming what holds it.
        let stood = self.switch_break.as_ref().is_some_and(|b| {
            b.phase == self.turn_end.wind().map_or("-", |w| w.phase.word())
                && !matches!(b.phase, "holding" | "-")
                && now >= b.since + bound
        });
        if stood && !said {
            self.stood_still_said(draft, hold, now, opts)?;
        }
        if let TurnEndAction::Escalate { reason } = &action
            && self.switch_break.as_ref().is_some_and(|b| !b.escalated)
        {
            if let Some(b) = &mut self.switch_break {
                b.escalated = true;
            }
            self.escalate_point(&turn, &[], Some(reason), review)?;
        }
        if first {
            review.note(&format!(
                "BACKGROUND seq={} the turn ended under a background terminal: a point for the \
                 switch's own steps",
                screen.seq
            ));
        }
        self.turn_end_execute(&turn, action, opts, &[], review)?;
        Ok(true)
    }

    /// THE STOOD-STILL NOTE ([`Self::switch_at_background`]): what holds
    /// the switch — the policy's own wait where one does (`hold`,
    /// [`TurnEndState::switch_hold`]: the footer, a sandbox, the restores
    /// spent), else a draft standing (`draft`), a person's keystroke within
    /// the grace (their hand read fresh, `status`), or the terminal — handed
    /// to the policy to be said once ([`TurnEndState::switch_stood_still`]).
    fn stood_still_said(
        &mut self,
        draft: bool,
        hold: Option<StoodBy>,
        now: Instant,
        opts: &SuperviseOpts,
    ) -> Result<(), Fail> {
        let by = match hold {
            Some(own) => own,
            None if draft => StoodBy::Draft,
            None => {
                self.program = self.foreground_program()?;
                let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
                if self.person_ago(now).is_some_and(|ago| ago < grace) {
                    StoodBy::Typing
                } else {
                    StoodBy::Terminal
                }
            }
        };
        self.turn_end.switch_stood_still(by);
        Ok(())
    }

    /// A Codex thread that fell into a sandbox its launch bypassed
    /// ([`TurnEndReading::sandbox_fell`]): said ONCE per fall, with the fix —
    /// nothing is typed into it (the turn-end policy's), an open save-then-
    /// wait switch's steps included.
    fn say_sandbox_fall(&mut self, point: &Turn, review: &mut dyn Review) -> Result<(), Fail> {
        let codex = self.reader(&point.screen.rows).program() == aterm_phase::Program::Codex;
        let fell = self
            .codex_seen
            .as_ref()
            .map(|(_, seen)| seen.sandbox_fell.clone())
            .or_else(|| {
                self.codex_fixed
                    .as_ref()
                    .map(|seen| seen.sandbox_fell.clone())
            })
            .flatten()
            .filter(|_| codex);
        let Some(policy) = fell else {
            self.sandbox_said = None;
            return Ok(());
        };
        if self.sandbox_said.as_deref() == Some(policy.as_str()) {
            return Ok(());
        }
        self.sandbox_said = Some(policy.clone());
        let switch = self.turn_end.wind().map_or_else(String::new, |w| {
            format!(
                " — not the save instruction, `/goal pause` or `/model` of its switch to {} \
                 either, so it stays there until then",
                w.to
            )
        });
        let text = format!(
            "this Codex thread now runs in the `{policy}` sandbox although it was launched \
             without one, so it cannot commit or push; aterm types nothing into it{switch}. Fix: \
             quit Codex and resume the thread with its launch flags (codex resume \
             --dangerously-bypass-approvals-and-sandbox <thread>)"
        );
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &text);
        self.escalate(point.screen.seq, &attention, None, "ask", review)?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// The standing rules, read now (an edit since the launch counts), as
    /// one line: `policy.rules_file`. `None` when it is not set, or the file
    /// cannot be read or is empty.
    fn rules_text(&self, opts: &SuperviseOpts) -> Option<String> {
        let path = opts.policy.rules_file.clone()?;
        let text = std::fs::read_to_string(&path).ok()?;
        let line = capped_rules(&limit::one_line(&text));
        (!line.is_empty()).then_some(line)
    }

    /// A wall's `reset=` text as the loop's clock, read once per text: the
    /// same text is the same reset ([`Self::refresh_reset`]'s rule). A reset
    /// already past is that long ago.
    fn wall_reset_at(&mut self, text: Option<&str>) -> Option<Instant> {
        let key = text.unwrap_or("-").to_string();
        if let Some((k, at)) = &self.wall_reset
            && *k == key
        {
            return *at;
        }
        let at = self.reset_unix(text).map(|unix| {
            let now = Instant::now();
            let delta = unix - self.now_unix();
            let d = Duration::from_secs(delta.unsigned_abs());
            if delta >= 0 {
                now + d
            } else {
                now.checked_sub(d).unwrap_or(now)
            }
        });
        self.wall_reset = Some((key, at));
        at
    }

    /// How long ago a person last typed into the session, as the policy
    /// reads it ([`TurnEndReading::person`]): the later of the server's last
    /// status (`human_ms=`) and the draft in the composer last changing.
    pub(super) fn person_ago(&self, now: Instant) -> Option<Duration> {
        self.person_at()
            .map(|typed| now.saturating_duration_since(typed))
    }

    /// When a person last typed into the session ([`Self::person_ago`]).
    pub(super) fn person_at(&self) -> Option<Instant> {
        let draft = self.draft_seen.as_ref().map(|(_, at)| *at);
        self.person.max(draft)
    }

    /// The turn-end reading of `screen`: the reader for the session's
    /// program (as last read), the work since the last point, the wall's
    /// reset, the rules, and how long ago a person typed — a draft that
    /// changed on any read included ([`Self::person_ago`]). At a wall the
    /// network answers ([`answered_by_the_network`]) — and there alone — the
    /// host's measure of the API's reach ([`IdleHost::reach`]), remembered
    /// as [`Self::reach_seen`]; everywhere else nothing is asked, and it is
    /// forgotten.
    fn turn_end_reading(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        let reader = self.reader(&screen.rows);
        let reading = reader.read(&screen.rows, Some(screen.cursor_col));
        let reset_at = match &reading.wall {
            Some(w) => {
                let text = w.reset.clone();
                self.wall_reset_at(text.as_deref())
            }
            None => None,
        };
        self.reach_seen = reading
            .wall
            .as_ref()
            .filter(|w| answered_by_the_network(w.kind))
            .map(|_| {
                opts.idle_host
                    .as_ref()
                    .map_or(Reach::Unknown, |h| h.reach())
            });
        let r = TurnEndReading::of(
            &reading,
            &screen.rows,
            typed_draft(reader, screen) || self.homed_draft(reader, screen),
            worked,
            reset_at,
            self.rules_text(opts),
        );
        let seen = if reading.program == aterm_phase::Program::Codex {
            self.note_codex_screen(&screen.rows);
            self.codex_records()
        } else {
            CodexSeen::default()
        };
        let host = opts.idle_host.as_ref();
        // The thread's model counts for an open switch only for a turn
        // begun since its press (else its opening), within a second.
        let pressed = self
            .turn_end
            .wind()
            .and_then(|w| w.pressed_at.or(w.opened_at))
            .map(|at| self.unix_of(at));
        let thread_model = codex_usage::model_since(&seen, pressed);
        TurnEndReading {
            reach: self.reach_seen.unwrap_or_default(),
            person: self.person_ago(Instant::now()),
            limits: seen.limits,
            sandbox_fell: seen.sandbox_fell,
            upgrade_goal: r.program == aterm_phase::Program::Codex && self.goal_held_by_upgrade(),
            thread_model,
            // Never while a limit episode stands: the upgrade types nothing
            // at a limit, and the wall is the loop's to wait out
            // ([`crate::supervise::IdleHost::limited`]); nor while Codex's
            // save-then-wait switch is open — the session is the switch's.
            upgrading: self.limit.is_none()
                && !self.turn_end.switch_open()
                && host.is_some_and(|h| h.owns_turn_end()),
            restartable: host.is_some_and(|h| h.can_restart()),
            resume: host.and_then(|h| h.resume_command()),
            // The screen's launch card, or the host's record of whose turns
            // the conversation holds (the harness's own are no task).
            taskless: r.taskless || host.is_some_and(|h| h.taskless()),
            ..r
        }
    }

    /// A draft the rows cannot tell from the placeholder: one row of text on
    /// the composer's caret row with the cursor at its column 2 — where both
    /// programs park it on an empty composer, and where a person's draft sits
    /// after its caret was moved home (←, Home, ctrl-a; measured on Claude
    /// Code 2.1.280 and 2.1.281). The `cell` there says which: the
    /// placeholder is drawn DIM ([`crate::supervise::screen::cell_is_dim`]), a typed
    /// draft is not. Only a reply that names no `dim` reads as a draft; an
    /// unreadable cell reads as the placeholder, as before.
    fn homed_draft(&mut self, reader: &dyn ScreenReader, screen: &Screen) -> bool {
        let Some((caret, lines)) = reader.composer(&screen.rows) else {
            return false;
        };
        let one_row = lines.first().is_some_and(|l| !l.is_empty())
            && lines.iter().skip(1).all(String::is_empty);
        if !one_row || screen.cursor_col != 2 || screen.cursor_index() != Some(caret) {
            return false;
        }
        let row = (screen.first + caret).to_string();
        match self.call(&["cell", &row, "2"]) {
            Ok(r) if r.ok() => !crate::supervise::screen::cell_is_dim(&r.stdout),
            _ => false,
        }
    }

    /// The decision at a NEW point: the point folded into the policy's
    /// memory ([`TurnEndState::observe`]) — once, here — then
    /// [`decide_turn_end`]; and when that is an act, decided again on a
    /// fresh `status` ([`Self::decide_to_act`]). A loop watching behind
    /// another supervisor's claim decides nothing: that one answers the
    /// session.
    pub(super) fn turn_end_at_point(
        &mut self,
        point: &Turn,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> Result<TurnEndAction, Fail> {
        if !self.held {
            self.turn_end_due = None;
        }
        if self.claim.watching_behind().is_some() {
            return Ok(TurnEndAction::Nothing);
        }
        let mut r = self.turn_end_reading(&point.screen, worked, opts);
        // Decided while the host owns the turn ends: decided again once it
        // owns nothing ([`Self::host_held`]).
        self.host_held = r.upgrading;
        let now = Instant::now();
        self.person_for_the_hold(&mut r, now)?;
        self.turn_end.observe(&r, now);
        self.decide_to_act(r, opts, now)
    }

    /// A HELD switch whose footer shows another model than its own: whose
    /// the change is — a person's `/model` since the hold began releases the
    /// switch, and is never driven back ([`TurnEndState::observe`]) — is
    /// judged on a person's last keystroke read FRESH (`status`), before the
    /// point is folded in.
    fn person_for_the_hold(&mut self, r: &mut TurnEndReading, now: Instant) -> Result<(), Fail> {
        let moved = self.turn_end.wind().is_some_and(|w| {
            matches!(w.phase, WindPhase::Holding { .. })
                && r.model_field.as_ref().is_some_and(|m| !m.same(&w.from))
        });
        if moved {
            self.program = self.foreground_program()?;
            r.person = self.person_ago(now);
        }
        Ok(())
    }

    /// [`decide_turn_end`] on `r`; when it says to type, the session's
    /// program and a person's last keystroke are read fresh (`status`,
    /// [`Self::foreground_program`]) and the point decided again on them —
    /// so a status read is paid only where there is something to type, and
    /// no act goes on an older word of a person than its own read.
    fn decide_to_act(
        &mut self,
        mut r: TurnEndReading,
        opts: &SuperviseOpts,
        now: Instant,
    ) -> Result<TurnEndAction, Fail> {
        let action = decide_turn_end(&self.turn_end, &r, &opts.policy, now);
        if action.rule_id().is_none() {
            return Ok(action);
        }
        self.program = self.foreground_program()?;
        r.person = self.person_ago(now);
        Ok(decide_turn_end(&self.turn_end, &r, &opts.policy, now))
    }

    /// The policy's `WaitUntil` ran out on the point still showing: read the
    /// screen again, and when it still shows the SAME point (its review key)
    /// fold it in again with no work ([`TurnEndState::observe`]: the same
    /// point read again changes nothing, except an act whose point has
    /// shown past [`TurnEndTiming::take_within`] with no busy read, which is
    /// judged now) and decide again, and carry it out; an escalation is the
    /// point's ([`Self::escalate_point`]). A screen that shows another point
    /// is left to the next look.
    pub(super) fn turn_end_now(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.turn_end_due = None;
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        // An act held for a person: the next look judges the point again.
        if std::mem::take(&mut self.held) {
            self.rejudge = true;
            return Ok(());
        }
        let screen = self.screen()?;
        let turn = self.turn_of(screen);
        if review_key(&turn, allow) != review_key(seen, allow) {
            return Ok(());
        }
        let mut r = self.turn_end_reading(&turn.screen, None, opts);
        self.host_held = r.upgrading;
        let now = Instant::now();
        self.person_for_the_hold(&mut r, now)?;
        self.turn_end.observe(&r, now);
        let action = self.decide_to_act(r, opts, now)?;
        if let TurnEndAction::Escalate { reason } = &action
            && matches!(turn.phase, Phase::Idle | Phase::Question)
        {
            self.escalate_point(&turn, allow, Some(reason), review)?;
        }
        self.turn_end_execute(&turn, action, opts, allow, review)
    }

    /// Carry out what the policy decided at `point` (module header). An act
    /// the server took is recorded in the policy's memory
    /// ([`TurnEndState::acted`]); one it did not take is not.
    pub(super) fn turn_end_execute(
        &mut self,
        point: &Turn,
        action: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        // What Codex's save-then-wait switch and its sandbox check left to
        // ledger and to say, before the point's act.
        self.wind_said(point, review)?;
        self.say_sandbox_fall(point, review)?;
        // The task ended at the done check: said once, to the journal alone
        // — nothing is typed from here until someone else's turn.
        let done = self.turn_end.task_done();
        if done && !self.task_done_said {
            review.note(&format!(
                "DONE seq={seq} rule={RULE_DONE_CHECK} the worker said it is done after the done \
                 check: nothing more is typed until a person or an orchestrator types"
            ));
        }
        self.task_done_said = done;
        let (text, rule) = match &action {
            TurnEndAction::Nothing | TurnEndAction::Escalate { .. } => return Ok(()),
            TurnEndAction::Restart {
                why,
                rule_id,
                otherwise,
            } => {
                return self.restart_agent(
                    point,
                    why,
                    rule_id,
                    otherwise.as_ref().clone(),
                    opts,
                    allow,
                    review,
                );
            }
            TurnEndAction::WaitUntil { until, why } => {
                let said = self.turn_end_due.as_ref().is_some_and(|(t, _)| t == until);
                if !said {
                    review.note(&format!(
                        "WAITING seq={seq} until={} {why}",
                        self.stamp_in(*until)
                    ));
                }
                self.turn_end_due = Some((*until, why.clone()));
                return Ok(());
            }
            TurnEndAction::Type { text, rule_id } | TurnEndAction::Accept { text, rule_id } => {
                (text.clone(), *rule_id)
            }
            TurnEndAction::TypeCommand {
                command, rule_id, ..
            } => (command.clone(), *rule_id),
            TurnEndAction::Submit { rule_id } => (
                self.reader(&point.screen.rows)
                    .composer(&point.screen.rows)
                    .map(|(_, lines)| lines.join(" "))
                    .unwrap_or_default(),
                *rule_id,
            ),
        };
        if !self.speaks_to_its_agent(seq, rule, review) {
            return Ok(());
        }
        // The switch's `/goal pause` over a goal the live upgrade already
        // holds paused for its move: the goal is the upgrade's — the switch
        // claims nothing of it (`crate::harness::goal_hold`).
        if rule == RULE_WIND_DOWN && text == GOAL_PAUSE && self.goal_held_by_upgrade() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the live upgrade holds Codex's goal paused for its \
                 move: the goal is not the switch's to pause"
            ));
            return Ok(());
        }
        // The `status` that named the program says the worker is not reading
        // its input: nothing is typed; the wait holds first (`stall.rs`).
        if self.stall_in_hand(review) {
            return Ok(());
        }
        let typed = match &action {
            TurnEndAction::Accept { .. } => self.accept_suggestion(point, &text, rule, review)?,
            TurnEndAction::Submit { .. } => self.submit_draft(point, &text, rule, review)?,
            _ => self.type_text(point, &text, rule, review)?,
        };
        if !typed {
            return Ok(());
        }
        let r = self.turn_end_reading(&point.screen, None, opts);
        self.turn_end.acted(&action, &r, Instant::now());
        // The switch's `/goal resume` at its reset over the live upgrade's
        // pause: the upgrade's resume, made by the switch — on its record,
        // so the upgrade verifies it and never makes its own.
        if rule == RULE_LIMIT_RESUME && text == GOAL_RESUME && r.upgrade_goal {
            self.goal_resume_pressed("switch");
        }
        self.mail_turn_boundary();
        let word = if matches!(action, TurnEndAction::TypeCommand { .. }) {
            "TYPED"
        } else {
            "CONTINUED"
        };
        review.say(&format!("{word} seq={seq} rule={rule} {}", clip(&text)))?;
        // An act of Codex's save-then-wait switch carries the switch as it
        // stands after it, so a later loop carries it on
        // ([`approvals::open_wind_down`]).
        let reason = self.wind_events_typed().map_or_else(
            || "the turn-end policy".to_string(),
            |w| format!("the turn-end policy {w}"),
        );
        self.ledger_row(rule, approvals::Outcome::Typed, &text, &reason, seq);
        self.wind_said(point, review)?;
        if let TurnEndAction::TypeCommand {
            then: Then::Escalate(reason),
            ..
        } = &action
        {
            let text = escalate::attention_text(self.reader(&point.screen.rows), point, reason);
            self.escalate(seq, &text, None, "ask", review)?;
            self.turn_end_badge = true;
        }
        Ok(())
    }

    /// THE RESTART the policy asked for ([`TurnEndAction::Restart`]: the
    /// memory banner, D3; a model bucket's fallback and its reset, D7): the
    /// session's host's ([`IdleHost::restart`]), journaled `HOST seq=<n>
    /// restart:<why> step=<word>`, and what the loop does with its word. The
    /// agent relaunched (`adopted`) is the loop's again at once — the host
    /// asks for the next idle point to carry it on — said `RESTARTED seq=<n>
    /// rule=<id> <what>`, remembered by the policy
    /// ([`TurnEndState::restarted`]: a bucket's switch, undone at its reset)
    /// and, for a model, ledgered as `relaunch --model <m>`, the row a later
    /// loop reads the open switch from ([`approvals::open_model_switch`]).
    /// One that must wait (`wait:…`: not idle yet, a person's hand, a job the
    /// relaunch cannot see yet; `busy:another-sweep`) is decided again after
    /// a pause that grows ([`TurnEndTiming::restart_backoff`]), never
    /// escalated for the wait. One that is never made — no host restarts
    /// here, a launch that cannot be carried, a signal refused — gets the
    /// point's `otherwise`: the memory banner's escalation (naming why), a
    /// bucket's reset waited out, the continuation a reset owed. Nothing
    /// while the worker's stall is in hand (`stall.rs`): the hold is first.
    #[allow(clippy::too_many_arguments)]
    fn restart_agent(
        &mut self,
        point: &Turn,
        why: &Restart,
        rule: &'static str,
        otherwise: TurnEndAction,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        if self.stall_in_hand(review) {
            return Ok(());
        }
        let step = opts.idle_host.as_ref().and_then(|h| h.restart(why));
        if let Some(step) = &step {
            review.note(&format!(
                "HOST seq={seq} restart:{} step={step}",
                why.word()
            ));
        }
        let word = step
            .as_deref()
            .map(|s| s.split_once(':').map_or(s, |(head, _)| head));
        match (word, step.as_deref()) {
            (Some("adopted" | "relaunched" | "continued" | "done"), _) => {
                self.restart_waits = 0;
                let r = self.turn_end_reading(&point.screen, None, opts);
                let now = Instant::now();
                self.turn_end.restarted(why, true, &r, now);
                // A host that typed the carry-on inside its restart typed a
                // turn of its own: its answer is awaited as the harness's,
                // never a short turn of the worker's.
                if word == Some("continued") {
                    self.turn_end.host_typed(now);
                }
                self.mail_turn_boundary();
                let what = match why {
                    Restart::Memory => "the agent, for its critical-memory banner".to_string(),
                    Restart::Model { to } => format!("relaunch --model {to}"),
                    Restart::ModelBack { to } => {
                        format!(
                            "relaunch --model {}",
                            to.as_deref().unwrap_or("(its launch's)")
                        )
                    }
                };
                let switch = self
                    .turn_end
                    .model_switch()
                    .filter(|_| matches!(why, Restart::Model { .. }))
                    .cloned();
                let back = match &switch {
                    Some(m) => format!(
                        " (from {}, back at {}; session-only)",
                        m.from.as_deref().unwrap_or("the launch's model"),
                        m.back_at
                            .map_or_else(|| "its reset".to_string(), |at| self.stamp_in(at))
                    ),
                    None => String::new(),
                };
                review.say(&format!("RESTARTED seq={seq} rule={rule} {what}{back}"))?;
                if !matches!(why, Restart::Memory) {
                    // A switch's row carries how to undo it, for a loop that
                    // starts before its reset.
                    let reason = match &switch {
                        Some(m) => approvals::model_switch_reason(
                            m.from.as_deref(),
                            m.back_at.map(|at| self.unix_of(at)),
                        ),
                        None => "the turn-end policy".to_string(),
                    };
                    self.ledger_row(rule, approvals::Outcome::Typed, &what, &reason, seq);
                }
                Ok(())
            }
            (Some("wait"), Some(step)) => self.restart_later(seq, why, step, review),
            (Some("busy"), Some(step)) if step == "busy:another-sweep" => {
                self.restart_later(seq, why, step, review)
            }
            _ => {
                self.restart_waits = 0;
                let r = self.turn_end_reading(&point.screen, None, opts);
                self.turn_end.restarted(why, false, &r, Instant::now());
                match otherwise {
                    TurnEndAction::Escalate { reason } => {
                        let reason = match &step {
                            Some(step) => {
                                format!("the restart could not be made ({step}): {reason}")
                            }
                            None => reason,
                        };
                        self.escalate_point(point, allow, Some(&reason), review)
                    }
                    other => self.turn_end_execute(point, other, opts, allow, review),
                }
            }
        }
    }

    /// A restart that must wait: decided again after the next pause of
    /// [`TurnEndTiming::restart_backoff`], `WAITING … until=<t>
    /// restart:<why> waits: <step>`.
    fn restart_later(
        &mut self,
        seq: u64,
        why: &Restart,
        step: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let pause = crate::supervise::ladder::Ladder(&self.turn_end.timing.restart_backoff)
            .step(usize::try_from(self.restart_waits).unwrap_or(usize::MAX));
        self.restart_waits = self.restart_waits.saturating_add(1);
        let until = Instant::now() + pause;
        let why = format!("restart:{} waits: {step}", why.word());
        review.note(&format!(
            "WAITING seq={seq} until={} {why}",
            self.stamp_in(until)
        ));
        self.turn_end_due = Some((until, why));
        Ok(())
    }

    /// Whether the session's foreground program — read fresh for this
    /// decision ([`Self::turn_end_at_point`], [`Self::turn_end_now`]) — is an
    /// agent this policy types into: one whose reader is measured
    /// ([`aterm_phase::Program::supervisable`] — Claude Code, Codex), never a
    /// shell or a program nothing reads. Not: journaled `SKIPPED`, nothing
    /// typed.
    fn speaks_to_its_agent(&mut self, seq: u64, rule: &str, review: &mut dyn Review) -> bool {
        let rows = self
            .last
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default();
        if self.reader(&rows).program().supervisable() {
            return true;
        }
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the session runs {}, no agent this policy types into",
            self.program.as_deref().unwrap_or("-")
        ));
        false
    }

    /// `text` typed and submitted under the guarded submit (module header):
    /// `true` when the server says it submitted it (`submitted=1`), or wrote
    /// the Enter unverified (`pressed=1`, journaled so). A guard that missed
    /// (`skipped`), a host without the guarded submit, a hold and a refusal
    /// are `false`, journaled; the hold is waited out and the refusal backed
    /// off, and the next look decides again.
    fn type_guarded(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        if self.caps.guarded_submit == Some(false) {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit"
            ));
            return Ok(false);
        }
        // The mark the composer is drawn with ON THIS SCREEN: Codex 0.158.0
        // draws its input line `»` where 0.157 drew `›` (measured
        // 2026-09-28), and a guard spelled with the other is never pressed
        // under — the text would be left typed and unsent.
        let Some(caret) = self.reader(&point.screen.rows).caret_on(&point.screen.rows) else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} no composer this policy types into"
            ));
            return Ok(false);
        };
        let submit = format!("submit=guarded:{}", composer_guard(caret, text));
        let r = self.call(&["turn", &submit, TURN_IDLE, TURN_TIMEOUT, TURN_YIELD, text])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!(
                "turn (the {rule} text) failed: {}",
                r.stderr.trim()
            )));
        }
        let why = one_line(r.err_text());
        if r.is_err("halted") {
            self.park_on_hold(&why, seq, review)?;
            return Ok(false);
        }
        if r.is_err("busy") || r.is_err("rate") {
            self.back_off(&why, seq, None, review)?;
            return Ok(false);
        }
        // A person typing outlived the yield: nothing was typed. Decided
        // again shortly, where the draft (if one is left) types nothing.
        if r.is_err("yield") {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a person is typing: {why}"
            ));
            self.redecide_soon("a person was typing");
            return Ok(false);
        }
        if r.unknown_form() {
            self.caps.guarded_submit = Some(false);
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} this aterm is too old for a guarded submit: {why}"
            ));
            return Ok(false);
        }
        self.caps.guarded_submit = Some(true);
        if r.submitted() {
            return Ok(true);
        }
        let verdict = r.turn_verdict().unwrap_or("").to_string();
        if verdict.split_whitespace().any(|w| w == "pressed=1") {
            review.note(&format!(
                "UNVERIFIED seq={seq} rule={rule} Enter written, the submit not seen: {verdict}"
            ));
            return Ok(true);
        }
        let verdict = if verdict.is_empty() { why } else { verdict };
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} not submitted: {verdict}"
        ));
        // `turn` pastes before its guard is checked: a miss leaves the text
        // typed (`reason=guard`), in the composer or wherever the cursor
        // was. Said, never left silently for the next look to read as a
        // person's draft.
        if r.skipped() || verdict.contains("reason=guard") {
            self.left_typed(
                point,
                text,
                &format!("the guard missed ({verdict})"),
                review,
            )?;
        }
        Ok(false)
    }

    /// A fenced write the server skipped on the UNCHANGED screen (a bare
    /// `OK skipped`: the generation held and the guard matched no row) —
    /// nothing written, and a retry would miss the same way: journaled
    /// `SKIPPED … the guard matched no row …` and escalated, so the worker
    /// is not left silently waiting on an act that cannot land.
    fn not_written(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        guard: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = point.screen.seq;
        review.note(&format!(
            "SKIPPED seq={seq} rule={rule} the guard matched no row of the judged screen \
             ({guard}); nothing written"
        ));
        let reason = format!(
            "not typed: aterm could not find the prompt line: `{}`",
            clip(text)
        );
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(seq, &attention, None, "ask", review)?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// The point decided again [`REDECIDE_AFTER_SKIP`] from now
    /// ([`Self::turn_end_now`], on the deadline of the loop's own wait).
    fn redecide_soon(&mut self, why: &str) {
        self.turn_end_due = Some((Instant::now() + REDECIDE_AFTER_SKIP, why.to_string()));
    }

    /// Text this policy wrote and did not submit: escalated (the worker's
    /// `attention`, one ask), the badge cleared when the worker works.
    fn left_typed(
        &mut self,
        point: &Turn,
        text: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        // What happened first: the badge is cut to fit.
        let reason = format!("typed, not submitted ({why}): `{}`", clip(text));
        let attention = escalate::attention_text(self.reader(&point.screen.rows), point, &reason);
        self.escalate(point.screen.seq, &attention, None, "ask", review)?;
        self.turn_end_badge = true;
        Ok(())
    }

    /// `text` typed and submitted: [`Self::type_fenced`] where the host
    /// fences `send`, else [`Self::type_guarded`] (module header). A program
    /// that takes an Enter right behind typed text as a newline (Codex's
    /// paste guard, [`ScreenReader::paste_guard`]) is written FENCED too —
    /// its Enter held until the echo has been still [`PASTE_GUARD_SETTLE_MS`],
    /// as the guarded `turn` holds it (the hazards review of 2026-09-25: the
    /// unfenced `turn` pasted with no check of the program, so an agent that
    /// exited under it had the text land in the shell).
    fn type_text(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        match self.type_fenced(point, text, rule, review)? {
            Some(typed) => Ok(typed),
            None => self.type_guarded(point, text, rule, review),
        }
    }

    /// Whether the host's `send` takes the `if-gen=` fence (`help send`
    /// names it; an older `send` types a leading `if-gen=` as TEXT, so it is
    /// never sent unprobed).
    pub(super) fn send_fence_known(&mut self) -> Result<bool, Fail> {
        if let Some(known) = self.caps.send_gen {
            return Ok(known);
        }
        let r = self.call(&["help", "send"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("help send failed: {}", r.stderr.trim())));
        }
        let known = r.ok() && super::super::policy::server_fences_send(&r.stdout);
        self.caps.send_gen = Some(known);
        Ok(known)
    }

    /// THE FENCED WRITE (module header): `None` when the host or the read
    /// cannot fence it (no generation on the read, no composer, `key`/`send`
    /// without `if-gen=`) — the caller falls back to [`Self::type_guarded`];
    /// else whether the text was submitted.
    fn type_fenced(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<Option<bool>, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            return Ok(None);
        };
        if self.caps.key_if == Some(false)
            || !self.gen_fence_known()?
            || !self.send_fence_known()?
        {
            return Ok(None);
        }
        let fence = format!("if-gen={generation}");
        let guard = format!("if={}", super::super::policy::row_guard(&rows[caret]));
        let r = self.call(&["send", &fence, &guard, "--", text])?;
        match self.refused_press(&r, &format!("{fence} {guard} (send)"))? {
            None => {}
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                return Ok(Some(false));
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                // Refused outright (a usage line): nothing written; never
                // retried unfenced.
                self.caps.send_gen = Some(false);
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                return Ok(Some(false));
            }
            Some(_) => return Ok(Some(false)),
        }
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the write; nothing \
                 written"
            ));
            self.redecide_soon("the screen moved before the write");
            return Ok(Some(false));
        }
        if r.skipped() {
            // The screen was the one judged and the composer's guard matched
            // no row of it: a retry misses the same way. Nothing was written;
            // the point is the owner's, said — never a silent stall.
            self.not_written(point, text, rule, &format!("{fence} {guard}"), review)?;
            return Ok(Some(false));
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        let settle = if self.reader(rows).paste_guard() {
            PASTE_GUARD_SETTLE_MS
        } else {
            SETTLE_MS
        };
        self.wait(&["idle", settle], SETTLE_CAP)?;
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let held = reader
            .composer(&now.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(text))
            && typed_draft(reader, &now)
            && reader.prompt(&now.rows).is_none();
        let (true, Some(generation), Some(caret)) =
            (held, now.generation.clone(), caret_row(reader, &now.rows))
        else {
            let turn = self.turn_of(now);
            self.left_typed(
                &turn,
                text,
                "the composer does not show it as written",
                review,
            )?;
            return Ok(Some(false));
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let args = super::super::policy::key_args(&guard, "enter", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        let turn = self.turn_of(now);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(Some(true)),
            None => {
                self.left_typed(&turn, text, "the screen moved before Enter", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                self.left_typed(&turn, text, "the session is halted", review)?;
                Ok(Some(false))
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                self.left_typed(&turn, text, &why, review)?;
                Ok(Some(false))
            }
            Some(_) => Ok(Some(false)),
        }
    }

    /// The draft standing in the composer submitted as it is (module header):
    /// Enter, fenced on the judged read's generation and guarded on its caret
    /// row. `text` is the draft as read, for what is said. A host or a read
    /// without the fence gets nothing pressed — the draft is escalated,
    /// never submitted blind.
    fn submit_draft(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let fenced = match (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) {
            (Some(generation), Some(caret))
                if self.caps.key_if != Some(false) && self.gen_fence_known()? =>
            {
                Some((generation, caret))
            }
            _ => None,
        };
        let Some((generation, caret)) = fenced else {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} a draft stands in the composer and this host \
                 has no fenced Enter"
            ));
            let reason = format!(
                "a draft stands in the composer, not submitted: `{}`",
                clip(text)
            );
            let attention = escalate::attention_text(self.reader(rows), point, &reason);
            self.escalate(seq, &attention, None, "ask", review)?;
            self.turn_end_badge = true;
            return Ok(false);
        };
        let guard = super::super::policy::row_guard(&rows[caret]);
        let args = super::super::policy::key_args(&guard, "enter", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None if r.skipped_changed() => {
                // A keystroke since the read: decided again on the screen as
                // it is now.
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the screen moved before Enter; the draft \
                     stays"
                ));
                self.redecide_soon("the screen moved before the draft's Enter");
                Ok(false)
            }
            None if r.skipped() => {
                // The judged screen, and its caret row matched no row of it
                // ([`Self::type_fenced`]'s bare skip).
                self.not_written(point, text, rule, &args, review)?;
                Ok(false)
            }
            None => Ok(true),
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                Ok(false)
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                Ok(false)
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                Ok(false)
            }
            Some(_) => Ok(false),
        }
    }

    /// The worker's own suggestion accepted (module header): `right`, fenced
    /// on the judged read's generation and guarded on the suggestion's row;
    /// the screen read once it has settled; then — when it shows the
    /// composer holding exactly the suggestion, typed now (the cursor off
    /// the placeholder's column) — Enter, fenced on that read and guarded on
    /// that row. A host or a read without the generation fence types the
    /// suggestion's words instead ([`Self::type_guarded`]); so does a
    /// suggestion the accept key did not fill.
    fn accept_suggestion(
        &mut self,
        point: &Turn,
        text: &str,
        rule: &str,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            return self.type_text(point, text, rule, review);
        };
        if self.caps.key_if == Some(false) || !self.gen_fence_known()? {
            return self.type_text(point, text, rule, review);
        }
        let guard = super::super::policy::row_guard(&rows[caret]);
        let args = super::super::policy::key_args(&guard, "right", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None => {}
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                return Ok(false);
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                return Ok(false);
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                return Ok(false);
            }
            Some(_) => return Ok(false),
        }
        if r.skipped_changed() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before the accept key"
            ));
            self.redecide_soon("the screen moved before the accept key");
            return Ok(false);
        }
        if r.skipped() {
            // The judged screen, and its suggestion's row guard matched no
            // row of it ([`Self::type_fenced`]'s bare skip).
            self.not_written(point, text, rule, &args, review)?;
            return Ok(false);
        }
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let shows = caret_text(reader, &now.rows).as_deref() == Some(text);
        let filled = shows && typed_draft(reader, &now) && reader.prompt(&now.rows).is_none();
        let (Some(generation), Some(caret)) =
            (now.generation.clone(), caret_row(reader, &now.rows))
        else {
            return Ok(false);
        };
        if !filled {
            // The key did not fill it (a build without the accept key, the
            // suggestion gone): its words, typed — once, guarded.
            if !(shows && !typed_draft(reader, &now)) {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the composer no longer shows the suggestion"
                ));
                return Ok(false);
            }
            let turn = self.turn_of(now);
            return self.type_text(&turn, text, rule, review);
        }
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let args = super::super::policy::key_args(&guard, "enter", Some(&generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(true),
            None => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the composer moved before Enter; the \
                     suggestion stays filled"
                ));
                Ok(false)
            }
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                Ok(false)
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                Ok(false)
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
                Ok(false)
            }
            Some(_) => Ok(false),
        }
    }
}
