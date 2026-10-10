// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The turn-end policy in the loop: every point where a worker's turn ended
//! is decided by [`decide_turn_end`], the one turn-end decider, and the loop
//! only carries out what it says.
//!
//! Around the pure decision the loop adds what it cannot know: the screen
//! read by the reader for the session's program (`aterm-phase`), the work the
//! turn did (the first busy read since the last point, [`Session::running`]),
//! a wall's reset on the loop's clock (read once per notice text), and the
//! standing rules (the `rules_file`, cut at [`RULES_CAP`]). Nothing it types
//! goes without:
//!
//! * the session's foreground program read fresh (`status program=`), its
//!   reader a supervised agent's (Claude Code's or Codex's), and every read
//!   of the composer that reader's ([`Session::reader`]);
//! * the fenced write ([`Session::type_fenced`]), where the host fences
//!   `send` on the screen generation: `send if-gen=<the judged read>
//!   if=<its composer row> -- <text>` writes the text only while the screen
//!   is still the one judged (otherwise `OK skipped reason=changed` and the
//!   point is decided again shortly; a guard matching no row of the
//!   unchanged screen, a bare `OK skipped`, is escalated). A read must then
//!   show the composer holding exactly that text before Enter goes, fenced
//!   on that read (`key if-gen=… if=<the caret row> enter`), and for a
//!   program whose paste guard takes an Enter right behind typed text as a
//!   newline (Codex, [`aterm_phase::ScreenReader::paste_guard`]) once the
//!   echo has held still [`PASTE_GUARD_SETTLE_MS`]. Text written and not
//!   submitted is escalated;
//! * elsewhere, the guarded submit (`turn submit=guarded:<re> yield=0.2`):
//!   parked while a person types, then pasted, and Enter pressed only while
//!   the row holding the cursor (one the composer draws, never a shell's)
//!   still ends as the text does ([`composer_guard`]). The paste itself is
//!   not fenced; a miss (`skipped`) leaves the text typed, and is escalated.
//!   A host without either gets nothing typed;
//! * for the worker's own suggestion, the vendor's accept key: `right` on an
//!   empty composer with a suggestion shown fills it (Claude Code 2.1.280),
//!   fenced on the judged read's generation and guarded on the suggestion's
//!   row, then Enter fenced on the read that shows it filled. A host without
//!   the generation fence gets the suggestion's words typed instead;
//! * for a draft left standing ([`TurnEndAction::Submit`]), Enter alone,
//!   fenced on the judged read's generation and guarded on the caret row. A
//!   host without the fence gets nothing pressed: the draft is escalated.
//!
//! A `WaitUntil` is the deadline of the loop's wait for the screen to move
//! ([`Session::wait_for_next`]): a change decides again at the next look,
//! the deadline on the point still showing ([`Session::turn_end_now`]).
//! Every act is a ledger row under its rule id and a printed line
//! (`CONTINUED seq=<n> rule=<id> <text>`, or `TYPED seq=<n> rule=<id>
//! <command>` for a slash command); a wait is journaled `WAITING seq=<n>
//! until=<UTC> <why>` once per deadline, an act not taken `SKIPPED seq=<n>
//! rule=<id> <why>`, and a task done after the done check `DONE seq=<n>
//! rule=continue-done-check@v1 …`, once.
//!
//! THE ACCOUNTS PROBE IS NONE OF THESE ACTS
//! (`docs/SPEC-claude-accounts-probe-2026-10-01.md`). Where the session's
//! host claims one at a plain idle turn end, the LOOP types `/usage` itself,
//! reads the Settings panel it opens once it reads settled, hands the host
//! the sample and closes the panel with an Escape fenced by the panel's own
//! header row ([`Session::usage_probe`]) — a step between the host's step
//! and the point's act, which the decider never hears of: it books no act,
//! folds no panel screen into the policy's memory, and the point is decided
//! again after it ([`Session::after_probe`]). A panel the harness opened is
//! owed its close until a look sees it gone ([`Session::usage_panel_look`]),
//! across an outage and across loops. With no host that claims — no roster —
//! none of it runs, and not a request is added to what the loop sent before.
//! Where the roster says a probe is DUE and the loop does not run it, the
//! loop says which gate shut, once a point: `SKIPPED seq=<n>
//! rule=usage-probe@v1 gate=<word>` ([`Session::probe_shut`], the words
//! [`ProbeGate`]'s) — printed, so a hosted loop's host logs it.

use super::super::codex_usage::{self, CodexSeen, LimitRead};
use super::super::policy::approval::RULE_RATE_NUDGE_SWITCH;
use super::super::policy::approval::squashed;
use super::super::policy::exception::exception_for;
use super::super::policy::turn_end::{
    ANSWERS_OFF, AccountAnswer, AccountAt, CodexSetting, FINISHED, GOAL_PAUSE, GOAL_RESUME,
    GoalStop, ProbeGate, RULE_ACCOUNT, RULE_DONE_CHECK, RULE_FINISHED, RULE_LIMIT_RESUME,
    RULE_MODEL_RESTORE, RULE_USAGE_PROBE, RULE_WIND_DOWN, Restart, StoodBy, Then, WIND_DOWN_BOUND,
    WindDown, WindEvent, WindPhase, decide_turn_end, wind_phase_word,
};
use super::wall_confirm::Check;
use super::*;
use crate::harness::accounts::Sample;
use crate::harness::usage::{
    PanelRead, PanelWindow, USAGE_COMMAND, panel_sample, usage_panel_read,
};

/// How long one look at Codex's own records ([`Session::codex_records`]) is
/// taken as current: it reads a few hundred files' times and the ends of
/// the newest rollouts.
const CODEX_LOOK_EVERY: Duration = Duration::from_secs(60);

/// How long a save-then-wait switch may stand still (owed its save, winding
/// down, owed its model back) through one break under a Codex background
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

/// The furthest a wall's reset may jump on a REPRINT of the same words
/// before the jump is doubted: the ladder's last rung (30 minutes).
const REPRINT_JUMP_MAX_S: i64 = 30 * 60;

/// How many reprints a doubted jump is kept back for ([`REPRINT_JUMP_MAX_S`]).
pub(super) const REPRINT_JUMPS_MAX: u32 = 4;

/// How long past its placement a reset is doubted at all: two hours, the four
/// ladder tries of [`REPRINT_JUMPS_MAX`] (10, 30, 30, 30 minutes).
const REPRINT_HOLD_WINDOW_S: i64 = 2 * 3600;

/// The longest text the cursor-row guard names whole, as `^❯ <text>`: it
/// sits on the caret row of any composer 43 columns wide or more.
const GUARD_TAIL_CHARS: usize = 40;

/// The most of the text's last word a longer text's guard names: the tail
/// of a word too long for the composer's width is what its last row holds.
const GUARD_WORD_CHARS: usize = 16;

/// `yield=<floor>` on every `turn` this policy types: the verb parks before
/// typing until the target's typing momentum has fallen to the floor, so a
/// person typing is not pasted over. A yield that outlives the turn's
/// timeout types nothing (`ERR yield timeout`).
const TURN_YIELD: &str = "yield=0.2";

/// The standing rules ride in the continuation cut to this many characters
/// (at a word, `…` after): a continuation stays a line a composer holds as
/// typed text, not a pasted-text placeholder the fence cannot read back.
pub(super) const RULES_CAP: usize = 400;

/// A fenced write the server skipped (the screen moved between the judged
/// read and the write: a person's keystroke, a box): the point is decided
/// again this far off, on the deadline of the loop's own wait.
const REDECIDE_AFTER_SKIP: Duration = Duration::from_secs(2);

/// The longest the loop waits for the worker to react to its own write (the
/// continuation's echo, [`Session::type_fenced`]; the accept key's fill,
/// [`Session::accept_suggestion`]) before it reads the composer to judge the
/// write. The wait ends the moment the screen moves, so a worker that reacts
/// pays nothing; the bound only says when one that has not reacted at all is
/// judged anyway. Judged too early, the read sees the composer as it was
/// before the write, and an accept key not yet read would get its words
/// typed after it, submitting them doubled. A worker starved past this
/// bound is still judged on the old composer, and one that stalls mid-draw
/// is read once the screen is still for [`SETTLE_MS`] or [`SETTLE_CAP`] has
/// passed (a half-drawn composer is escalated).
pub(super) const REACTION_WAIT: Duration = Duration::from_secs(10);

/// How long the echo of a fenced write must hold still before its Enter, for
/// a program with a paste guard ([`ScreenReader::paste_guard`]: Codex takes
/// an Enter within a burst of typed text as a newline): the guarded `turn`'s
/// own settle (`idle=600`).
const PASTE_GUARD_SETTLE_MS: &str = "600";

/// The cursor-row guard for `text` typed into the composer whose caret row
/// starts with `caret` (the program's, [`ScreenReader::caret`]: `❯` Claude
/// Code, `›` Codex): `^<caret> <text>` to the row's end when the text is
/// short enough to sit on the caret row ([`GUARD_TAIL_CHARS`]); else the end
/// of its last word (at most [`GUARD_WORD_CHARS`]) to the row's end, on a
/// row the composer draws (its caret row, or a continuation row indented
/// two). Both programs wrap between words, so the row the cursor ends on
/// holds the last word at any width. Anchoring to the composer keeps the
/// guard off a shell's cursor row (a shell's row opens with its prompt, its
/// soft-wrapped continuation with the text itself), so an agent that exited
/// mid-write never has the text run as a command. Whitespace-free
/// ([`super::super::policy::row_guard`]'s escaping).
pub(crate) fn composer_guard(caret: char, text: &str) -> String {
    if text.chars().count() <= GUARD_TAIL_CHARS {
        // The caret's space is whatever the program draws there (a no-break
        // space on Claude Code 2.1.280 and 2.1.281: `❯\u{a0}keep going`), so
        // it is `\s`, never the `\x20` a typed space is.
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

/// After a Mac's wake, a wait of the policy's is decided
/// again this long later — the network is coming back (Wi-Fi re-associating,
/// a VPN reconnecting), and the first act typed into an API that cannot be
/// reached is a wall of its own — plus up to [`WAKE_SPREAD`] of the session's
/// own, so the sessions of one account, every one waking at the same
/// instant with its reset past, do not all type in the same second.
pub(super) const WAKE_SETTLE: Duration = Duration::from_secs(30);
/// See [`WAKE_SETTLE`].
pub(super) const WAKE_SPREAD: Duration = Duration::from_secs(60);

/// Whether the `tries`-th ask of a restart the host keeps deferring is
/// journaled: the first four (the pauses grow 10 s, 1, 5, 10 min) and then
/// every sixth — an hour on the last pause — where a fallback that waits on
/// the agent's own jobs for a day would otherwise write ~290 lines of the same
/// `wait:changed`. The pause itself is not journaled away: it is `turn_end_due`.
fn restart_note_due(tries: u32) -> bool {
    tries < 4 || tries.is_multiple_of(6)
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

/// A wall the host's measure of the API's reach answers: an API error that
/// never reached the API, a reply the connection cut off, or a certificate
/// or proxy refused ([`aterm_phase::ApiCause`]), the arms of the turn-end
/// policy that read [`TurnEndReading::reach`]. The server's own failure (a
/// status, an overload) is not: a handshake says nothing of the API's
/// health, and it keeps its ladder whatever the measure.
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

/// The walls a usage window's reset lifts.
fn resumes_at_reset(kind: aterm_phase::WallKind) -> bool {
    use aterm_phase::WallKind as K;
    matches!(
        kind,
        K::UsageSession | K::UsageWeekly | K::Spend | K::ModelBucket { .. }
    )
}

/// A session's stable share of `span`, from its id and the
/// reset (unix seconds) — FNV-1a, no clock and no randomness, so the same
/// session waits the same and the policy stays a pure function of what it is
/// handed. Zero for a zero span.
pub(super) fn resume_jitter(sid: &str, reset_unix: i64, span: Duration) -> Duration {
    let span_s = span.as_secs();
    if span_s == 0 {
        return Duration::ZERO;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sid.bytes().chain(reset_unix.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Duration::from_secs(h % span_s)
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

/// The unix second nearest `ahead_ms` past the unix millisecond `now_ms`: the
/// second `now_ms` falls in, moved by its own milliseconds plus `ahead_ms`,
/// rounded half away from that second. On a whole-second clock (a test's) that
/// is the second plus `ahead_ms` rounded half away from zero.
pub(super) fn nearest_s(now_ms: i64, ahead_ms: i64) -> i64 {
    let off = now_ms.rem_euclid(1000).saturating_add(ahead_ms);
    let whole = i64::try_from(off.unsigned_abs().saturating_add(500) / 1000).unwrap_or(i64::MAX);
    now_ms
        .div_euclid(1000)
        .saturating_add(off.signum().saturating_mul(whole))
}

impl<C: Ctl> Session<'_, C> {
    /// `t` on the loop's clock as unix seconds, the nearest, against the
    /// clock to the millisecond ([`Self::now_unix_ms`]).
    pub(super) fn unix_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        nearest_s(now_ms, ahead)
    }

    /// `t` on the loop's clock as unix MILLISECONDS ([`Self::unix_of`] to the
    /// millisecond): the wall time a deadline names ([`Due::unix_ms`]).
    pub(super) fn unix_ms_of(&self, t: Instant) -> i64 {
        let (now_ms, ahead) = self.ms_ahead(t);
        now_ms.saturating_add(ahead)
    }

    /// The wall clock now (unix milliseconds) and how many milliseconds
    /// `t` lies ahead of now on the monotonic clock (negative: behind).
    fn ms_ahead(&self, t: Instant) -> (i64, i64) {
        let now = Instant::now();
        let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
        let ahead = if t >= now { ms(t - now) } else { -ms(now - t) };
        (self.now_unix_ms(), ahead)
    }

    /// Stand `at` (on the loop's clock) as the policy's deadline at the point
    /// still showing ([`Self::turn_end_due`]), and why: kept on both clocks
    /// ([`Due`]), its wall time read now.
    pub(super) fn set_due(&mut self, at: Instant, why: impl Into<String>) {
        self.turn_end_due = Some(Due {
            at,
            unix_ms: self.unix_ms_of(at),
            why: why.into(),
        });
    }

    /// When the policy's deadline falls ([`Due`]), on the loop's clock now:
    /// its `Instant` — unless the wall clock has run ahead of the monotonic
    /// one since it was set by more than a sleep ([`super::slept_through`]),
    /// when it is the wall time it named, now behind or nearer. The
    /// milliseconds between the two clocks' reads never move it, so an
    /// awake wait never falls a moment before its `Instant` and is decided
    /// twice. `None`: nothing waited on.
    pub(super) fn due_at(&self) -> Option<Instant> {
        let due = self.turn_end_due.as_ref()?;
        let wall = self.instant_of_ms(due.unix_ms);
        // How much sooner the wall clock reads the deadline than the
        // monotonic one: the sleep since it was set.
        let ahead =
            i64::try_from(due.at.saturating_duration_since(wall).as_secs()).unwrap_or(i64::MAX);
        Some(if super::slept_through(ahead, 0) {
            wall
        } else {
            due.at
        })
    }

    /// `t` on the loop's clock as a UTC stamp, for the journal (a `t` already
    /// past stamps now). A unix time made an `Instant` by
    /// [`Self::instant_of`] stamps as that unix time, whenever it is read.
    pub(super) fn stamp_in(&self, t: Instant) -> String {
        let secs = self.unix_of(t.max(Instant::now()));
        utc_stamp(u64::try_from(secs).unwrap_or(0))
    }

    /// A Codex screen's footer, kept: the model and effort, and the goal. A
    /// box covers the footer, and the rate-limit nudge is answered by what the
    /// footer showed before it.
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

    /// What Codex's own records say of this session ([`codex_usage::look`]):
    /// its usage window, and whether its thread fell into a sandbox its launch
    /// bypassed, under the `$CODEX_HOME` the session's foreground process (its
    /// Codex TUI) runs with, read through the kernel as the approval policy
    /// reads the worker ([`Self::worker_env`]). The thread that decides is the
    /// session's own where known ([`codex_usage::own_thread`]: the writer lock
    /// the TUI holds open, else the thread its argv resumes), else the
    /// rollouts written around the session's last work
    /// ([`Session::last_busy_at`]; for a hold carried on from the ledger, the
    /// hold's start). One look is current for [`CODEX_LOOK_EVERY`]. A process
    /// that cannot be read, or names no home, reads unknown.
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

    /// `unix` (epoch seconds) on the loop's clock: placed against the clock to
    /// the millisecond ([`Self::now_unix_ms`]), so the `Instant` is `unix`
    /// itself, whatever part of a second the read fell in. The ONE way a
    /// unix time becomes an `Instant` here; [`Self::unix_of`] is its inverse.
    pub(super) fn instant_of(&self, unix: i64) -> Instant {
        self.instant_of_ms(unix.saturating_mul(1000))
    }

    /// [`Self::instant_of`] for a unix MILLISECOND ([`Self::unix_ms_of`]'s
    /// inverse). A time further back than the monotonic clock reaches is now.
    pub(super) fn instant_of_ms(&self, unix_ms: i64) -> Instant {
        let now = Instant::now();
        let delta = unix_ms.saturating_sub(self.now_unix_ms());
        let d = Duration::from_millis(delta.unsigned_abs());
        if delta >= 0 {
            now + d
        } else {
            now.checked_sub(d).unwrap_or(now)
        }
    }

    /// The model switch an earlier loop left open
    /// ([`approvals::open_model_switch`]), carried on by this one: its reset
    /// placed on this loop's clock by [`Self::instant_of`], so the switch back
    /// and the row that says when stand at the second the earlier row named.
    pub(super) fn carried_model_switch(&self, open: approvals::OpenSwitch) -> ModelSwitch {
        ModelSwitch {
            from: open.from,
            to: open.to,
            back_at: open.back_at_unix.map(|unix| self.instant_of(unix)),
        }
    }

    /// The Codex wind-down an earlier loop left open
    /// ([`approvals::open_wind_down`]), carried on by this one: every time its
    /// rows name (the reset, the hold's start, the last Esc, the opening, the
    /// press, the last row) placed on this loop's clock by
    /// [`Self::instant_of`].
    pub(super) fn carried_wind(&self, open: approvals::OpenWind, now: Instant) -> WindDown {
        open.into_wind(|unix| self.instant_of(unix), now)
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

    /// The tab's goal record ([`crate::harness::goal_hold`]) beside this
    /// loop's ledger: the one file the live upgrade's pause and this switch
    /// both keep their hold of Codex's goal in; `None` with no ledger.
    fn goal_hold_path(&self) -> Option<std::path::PathBuf> {
        self.ledger_path
            .as_deref()
            .map(crate::harness::goal_hold::path_beside)
    }

    /// Whether the live upgrade holds Codex's goal paused for its move (its
    /// hold on the tab's goal record still owes the goal its resume): the
    /// switch then claims nothing of that goal (no `/goal pause`, no Esc into
    /// the turn the pause lets finish, since no running tool call is cut off),
    /// and the goal stays the upgrade's to resume.
    pub(super) fn goal_held_by_upgrade(&self) -> bool {
        self.goal_hold_path()
            .is_some_and(|p| crate::harness::goal_hold::held_by_upgrade(&p))
    }

    /// Whether the paused-goal box on screen is the live upgrade's to answer
    /// ([`crate::harness::goal_hold::box_is_upgrades`]): its hold owes the
    /// resume, the Codex it relaunched for the move still leads its terminal
    /// ([`Session::codex_leads`], the kernel's word), and the box was not left
    /// to a person; never another Codex's box, such as a person's own `codex
    /// resume` in the tab.
    pub(super) fn goal_box_is_upgrades(&self) -> bool {
        let leads = self.codex_leads;
        self.goal_hold_path()
            .is_some_and(|p| crate::harness::goal_hold::box_is_upgrades(&p, leads))
    }

    /// The live upgrade's resume, made by this loop, and `why`: the paused
    /// goal's box the relaunched Codex opened with, answered with its resume
    /// under `goal-resume@v1` (the approval policy's `goal_resume_pick`:
    /// `moved`), or the save-then-wait switch's `/goal resume` at its reset
    /// over a pause of the upgrade's (`switch`), written to the upgrade's hold
    /// on the tab's goal record as its step would write it (`Resuming`: made,
    /// its showing on the footer for the upgrade's next look to verify), so it
    /// is never made twice.
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

    /// The switch's claim kept in step with its own state
    /// ([`crate::harness::goal_hold::sync_switch`]): while the switch holds
    /// Codex's goal paused ([`WindDown::goal_paused`]), its hold is on the
    /// tab's goal record, where the live upgrade reads it and never pauses the
    /// goal over it, and it ends there as the switch lets the goal go. Nothing
    /// is written while nothing changed.
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

    /// The switch the nudge's press will open (the approval policy's
    /// `rate-nudge-switch@v1`, its option `label`), made before the key goes:
    /// from the model the footer last showed, back at the window's reset
    /// Codex's records name, with a one-time marker, stamped as pressed now
    /// ([`WindDown::pressed_at`]: the floor under the thread's rollout). Its
    /// words are the press's intent row, ledgered before the key
    /// ([`Self::nudge_intended`]), so a loop that dies between the key and its
    /// record still carries the switch on; it opens only once the box has
    /// left ([`TurnEndState::open_switch`]). `None` when the footer was never
    /// read.
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
    /// read, else (a loop that started at the box, which covers the footer)
    /// the `from` of a press only intended that it carried on
    /// ([`WindDown::intent`]).
    pub(super) fn nudge_from(&self) -> Option<CodexSetting> {
        self.codex_setting.clone().or_else(|| {
            self.turn_end
                .wind()
                .filter(|w| w.intent)
                .map(|w| w.from.clone())
        })
    }

    /// The press's intent, ledgered before its key (`phase=intent`): a row
    /// [`approvals::open_wind_down`] seeds from as only intended
    /// ([`WindDown::intent`]), so a loop restarted before the key went decides
    /// the nudge still up again, and one restarted after it sees the switch
    /// land on the footer.
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

    /// The nudge's press did not land (the box did not change, the key was
    /// never sent): the intent is closed (`phase=released`), and nothing of the
    /// switch goes on.
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

    /// Whether the turn running now is a person's
    /// ([`RunningTurn::person`]): a keystroke of theirs, or a draft that
    /// changed, since the open switch opened ([`WindDown::opened_at`]) and
    /// within the turn's own work so far ([`Session::running`]: from its first
    /// busy read after the last point, a break, or the nudge's box), or, for
    /// the turn in flight when this loop carried a switch on, since its last
    /// row and within [`SEEDED_TURN_BOUND`] of the loop's first read
    /// ([`WindDown::seeded_at`]), latched until the turn's point; or one within
    /// `human_grace_s`, which holds the stop while it lasts.
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

    /// The nudge's box answered (its switch pressed, its keep): the turn it
    /// covered ended under it, so the turn running now (Codex's goal turn,
    /// started under the box within milliseconds) is measured from its own
    /// first busy read, and a person's hand latched before is none of it
    /// ([`RunningTurn::boxed`]).
    ///
    /// [`RunningTurn::boxed`]: super::super::policy::turn_end::RunningTurn::boxed
    pub(super) fn nudge_answered(&mut self) {
        self.running.boxed();
    }

    /// The save-then-wait switch opens (the nudge's press seen landing, or
    /// carried as only intended): `wind` stamped with its opening, the floor
    /// under every person's keystroke the switch reads and under the thread's
    /// rollout, after the box's span is closed ([`Self::nudge_answered`]).
    pub(super) fn open_wind(&mut self, wind: WindDown) {
        let now = Instant::now();
        self.nudge_answered();
        self.turn_end.open_switch(WindDown {
            opened_at: wind.opened_at.or(Some(now)),
            ..wind
        });
    }

    /// Codex's turn stopped while the switch is open (nothing goes on on the
    /// cheaper model; [`TurnEndState::goal_stop`]): read at the press's
    /// landing and at every busy read of an unattended loop. A turn running
    /// while the wind-down is owed or the model owed back (the goal turn Codex
    /// starts under the nudge's box within milliseconds of the turn's end, or
    /// one its goal starts again after a stop), or a goal turn during the
    /// hold, that is no person's (no keystroke of theirs since the switch
    /// opened within the grace or the turn's own work so far, and no busy read
    /// of it that saw their hand, [`Self::person_in_this_turn`]; read fresh
    /// from `status` before the key) gets one Esc guarded on its status row;
    /// Esc stops the turn and pauses the goal at once. Up to [`GOAL_STOPS`]
    /// stops a switch; past them the goal's next turn is said to a person,
    /// once. The save's own turn gets its one Esc once its busy work since the
    /// save was typed (the loop's span, [`Session::running`], plus the save's
    /// turns carried on past a wall) reaches [`WIND_DOWN_BOUND`], since the
    /// cheaper model only commits and pushes; a save still running after it
    /// is said, once. A status row no longer up sends nothing. Journaled
    /// `INTERRUPTED`, ledgered under `model-wind-down@v1`.
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
        // The live upgrade holds Codex's goal paused for its move: a goal's
        // turn running is the one its pause lets finish, never stopped. The
        // switch's own save turn (winding down) is not: its bound stands.
        let winding = self
            .turn_end
            .wind()
            .is_some_and(|w| w.phase == WindPhase::Winding);
        if self.goal_held_by_upgrade() && !winding {
            return Ok(());
        }
        let guard = reader
            .busy_guard()
            .filter(|g| self.busy_up(g, &screen.rows) && reader.phase(&screen.rows) == Phase::Busy);
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

    /// The switch's events since the last look: the words of the last typed
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

    /// The switch's edges ledgered (`skipped`, carrying the switch), its notes
    /// said, once each, as the point's escalation ([`escalate`]'s `ask`, the
    /// badge standing until the worker works); the note that Codex's goal runs
    /// on after every stop is kept up while the switch stands and the goal
    /// runs ([`Session::goal_badge`]), and recorded for a person to see, as is
    /// the hold's start ([`Self::hold_said`]).
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

    /// A note of the switch's, said: its own `skipped` row, carrying the
    /// switch as it stands with its notes said among its words (`told=`), so a
    /// restarted loop raises none of them again, even a note said where the
    /// switch then stands still and writes no other row. A switch the point
    /// closed has its closing row.
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

    /// The hold, made visible (it can last days): one record when it begins,
    /// in plain words (the model and effort it waits on and when its limit
    /// resets, or that nobody read the reset and the latest it lasts), said on
    /// the loop's journal (`HOLDING`), ledgered, and handed to the session's
    /// host as information for a person (the window's Messages, no badge:
    /// [`crate::supervise::IdleHost::inform`]). The reading that ends the hold
    /// early is Codex's own usage record, which a held session no longer
    /// writes: an early reset is seen only when another Codex session on the
    /// account writes one, so the end named is the reset's.
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

    /// A turn's end under a Codex background terminal while the save-then-wait
    /// switch is open: Codex draws `1 background terminal running · /ps to
    /// view` under an ended turn, and the screen reads busy for as long as the
    /// terminal runs, so no point would come and the thread would sit on the
    /// cheaper model. Here such a read (the screen without the line, an ended
    /// turn read authoritatively however it ended,
    /// [`aterm_phase::codex::ended_under_background`]: idle, a question, a wall
    /// or an API error) is a point for the switch's own steps and nothing
    /// else: the screen, its line blanked, is folded into the policy as the
    /// turn's end ([`TurnEndState::observe`], the turn's work taken once per
    /// break) and decided as an ordinary point ([`decide_turn_end`]): the
    /// marker judged, `/model`, the hold's clock, its resume, a wall's own
    /// rule where the switch leaves it one, the point's escalation raised once
    /// a break. No host step, report or continuation, and nothing once the
    /// point has closed the switch. A wait it names is kept
    /// ([`Session::turn_end_due`]) and the break decided again at it. A switch
    /// that has not moved past an owed save, a wind-down or a restore for
    /// [`SWITCH_BREAK_NOTE`] of one break is said to a person, once, naming
    /// what holds it (the policy's own wait, [`TurnEndState::switch_hold`]; a
    /// person's typing; a draft; the terminal); on the same break read again
    /// before the wait its decision named, it is only said, and the act and
    /// wait that decision named stand. `true` when the read was taken as the
    /// switch's: it is no work of the worker's.
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
        let key = self.review_key(&turn);
        let bound = self.switch_break_note;
        let said = self
            .turn_end
            .wind()
            .is_some_and(|w| w.said.contains(&"background"));
        let due = self.due_at();
        let worked = match &mut self.switch_break {
            Some(b) => {
                if b.phase != phase {
                    b.phase = phase;
                    b.since = now;
                }
                // The same break read again decides nothing new before the
                // wait its last decision named, as the same point read again
                // at an ordinary turn end. A switch that has stood still
                // through it past the bound is said there, and only said.
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
        self.account_round();
        let mut r = self.turn_end_reading(&ended, worked, opts);
        self.turn_end_due = None;
        self.person_for_the_hold(&mut r, now)?;
        self.turn_end.observe(&r, now);
        self.account_advise(&mut r, ended.seq, opts);
        let draft = r.composer.drafted();
        let hold = self.turn_end.switch_hold(&r);
        if let Some(b) = &mut self.switch_break {
            b.draft = draft;
            b.hold = hold;
        }
        // The point closed the switch (a person's `/model`, a switch that did
        // not land): the break is no longer the switch's, and nothing else is
        // done at it; its edges are ledgered.
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
            self.escalate_point(&turn, Some(reason), review)?;
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

    /// The stood-still note ([`Self::switch_at_background`]): what holds the
    /// switch (the policy's own wait, `hold`, [`TurnEndState::switch_hold`];
    /// else a draft standing, `draft`; a person's keystroke within the grace,
    /// read fresh from `status`; or the terminal), handed to the policy to be
    /// said once ([`TurnEndState::switch_stood_still`]).
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
    /// ([`TurnEndReading::sandbox_fell`]): said once per fall, with the fix;
    /// nothing is typed into it, an open save-then-wait switch's steps
    /// included.
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

    /// A wall's `reset=` text as the loop's clock, read once per text and
    /// kind: the same text is the same reset ([`Self::refresh_reset`]'s rule).
    /// A reset already past is that long ago. The
    /// reset is placed for the wall's kind ([`Self::reset_unix_for`]) and
    /// remembered as UNIX time, turned into the loop's monotonic clock at
    /// every reading — an `Instant` does not count the time a Mac sleeps
    /// (measured here: `CLOCK_UPTIME_RAW` runs 2.4 days behind
    /// `CLOCK_MONOTONIC`), so one cached at the first reading lands late by
    /// every sleep since. The cache is dropped with the wall
    /// ([`Self::turn_end_reading`]), so the next wall of the same words is a
    /// new reset.
    pub(super) fn wall_reset_at(
        &mut self,
        text: Option<&str>,
        kind: aterm_phase::WallKind,
        session_window: bool,
        live: bool,
        answered: bool,
        rows: &[String],
    ) -> Option<Instant> {
        let key = format!("{}|{}", kind.name(), text.unwrap_or("-"));
        // Only a bare clock time on a Claude Code wall that is no session
        // window is placed on the print (a span counts from its first print
        // and is read once; a dated time and a session window's clock do not
        // move with it; a Codex's words keep their first placement, the
        // host's or the look's day's — [`Self::reset_unix_for`]).
        let reprint = (live || answered)
            && !session_window
            && self.reader(rows).program() != aterm_phase::Program::Codex
            && matches!(
                text.and_then(limit::parse_reset),
                Some(limit::ResetSpec::At { date: None, .. })
            );
        let unix = match &self.wall_reset {
            // The same print read again (a wait's wake, a re-decision, the
            // screen changing under it): the reset it was placed at. NOT a
            // print the loop watched arrive (`live`): after an act of ours
            // the wall answered, and the notice printed again — the same
            // words, a new print — is placed afresh on what the loop saw. A
            // reset guessed for a wall found sitting was wrong once at most
            // (cached under the words, the same misplacement
            // answered every retry: 26 blind messages in a 13-hour wait, in a
            // replay of the policy).
            Some((k, u)) if *k == key && !reprint => *u,
            _ => {
                let same = self
                    .wall_reset
                    .as_ref()
                    .filter(|(k, _)| *k == key)
                    .and_then(|(_, o)| *o);
                let mut u = self.reset_unix_for(text, session_window, live, answered, rows);
                // A reprint placed on
                // ITS OWN print can jump a day: `resets 4pm`, tried at 16:02
                // and 16:12, reprinted at 16:13 — placed on 16:07, that is
                // TOMORROW's 16:00, a 23-hour wait for a lift that lagged
                // twelve minutes. A jump past the ladder's last rung on the
                // same words, from a placement that passed within the last
                // two hours, is not taken at once: the ladder goes on (at
                // most [`REPRINT_JUMPS_MAX`] times — four more tries, about
                // two hours — and then the far placement is believed, since
                // the vendor prints a bare clock only for a reset within a
                // day AHEAD).
                let now_unix = self.now_unix();
                match (reprint, same, u) {
                    // Only a placement that PASSED recently is doubted: the
                    // reset was lately behind us and the wall still stands.
                    // One placed hours back was a guess (a wall found sitting
                    // with no print to go by), and its reprint is the
                    // evidence.
                    (true, Some(old), Some(new))
                        if new > old.saturating_add(REPRINT_JUMP_MAX_S)
                            && now_unix.saturating_sub(old) <= REPRINT_HOLD_WINDOW_S
                            && self.reprint_jumps < REPRINT_JUMPS_MAX =>
                    {
                        self.reprint_jumps += 1;
                        u = Some(old);
                    }
                    (_, None, _) => self.reprint_jumps = 0,
                    _ => {}
                }
                self.wall_reset = Some((key, u));
                u
            }
        };
        unix.map(|unix| self.instant_of(unix))
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

    /// The turn-end reading of `screen`: the reader for the session's program
    /// (as last read), the work since the last point, the wall's reset, the
    /// rules, and how long ago a person typed, a draft that changed on any
    /// read included ([`Self::person_ago`]). At a wall the network answers
    /// ([`answered_by_the_network`]), and there alone, the host's measure of
    /// the API's reach ([`IdleHost::reach`]), remembered as
    /// [`Self::reach_seen`]; everywhere else nothing is asked, and it is
    /// forgotten.
    ///
    /// A DECISION's read: the screen's wall is checked against the session's
    /// transcript here ([`Self::confirm_wall`], [`Check::Decide`]). The
    /// point read again for the loop's books after its act is
    /// [`Self::turn_end_reread`].
    pub(super) fn turn_end_reading(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, worked, opts, Check::Decide)
    }

    /// The reading of a point ALREADY DECIDED, for the loop's books after
    /// its act — what the act was taken at, the restart made or waited for:
    /// its wall as the decision's read judged it at that screen
    /// ([`Check::AsDecided`]). The transcript, moved on by the act itself
    /// (the carry-on typed, the relaunched conversation), is not asked again:
    /// asked, it refused the wall just acted on.
    pub(super) fn turn_end_reread(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
    ) -> TurnEndReading {
        self.turn_end_reading_as(screen, None, opts, Check::AsDecided)
    }

    fn turn_end_reading_as(
        &mut self,
        screen: &Screen,
        worked: Option<Duration>,
        opts: &SuperviseOpts,
        check: Check,
    ) -> TurnEndReading {
        let reader = self.reader(&screen.rows);
        let mut reading = reader.read(&screen.rows, Some(screen.cursor_col));
        // The screen's wall acted on only where the session's transcript
        // confirms it (H6-HARNESS-1): refused, the point is an ordinary turn
        // end — before anything below reads the wall.
        self.confirm_wall(&mut reading, screen, opts, check);
        // The wall waited on, remembered while its rows are
        // out of view; another wall or an idle screen ends it.
        self.wall_memory_step(reader, &screen.rows, &mut reading, screen.seq);
        let reset_at = match &reading.wall {
            Some(w) => {
                let text = w.reset.clone();
                // A session window says so (`session limit`, `5-hour`): five
                // hours at most. A `Usage limit reached · …` that names no
                // window is placed on the print like any other wall's.
                let session_window = w.kind == aterm_phase::WallKind::UsageSession
                    && limit::names_a_session_window(&w.message);
                // A wall the loop WATCHED: one a turn it saw run ended on, or
                // one standing where an act of its own went untaken past its
                // window — the wall answered that act, so the limit stands
                // now ([`TurnEndState::untaken_act`]). Without the second, a
                // `5am` found at 17:26 with no print to go by was placed by the
                // session rule in the past, and the `keep going` it was
                // answered with — held by Claude Code, no turn, no busy read —
                // left that placement standing: typed again every 30 minutes
                // until the reset. Once an act.
                let answered = self
                    .turn_end
                    .untaken_act(Instant::now())
                    .filter(|at| self.wall_answered != Some(*at));
                if answered.is_some() {
                    self.wall_answered = answered;
                }
                self.wall_reset_at(
                    text.as_deref(),
                    w.kind,
                    session_window,
                    worked.is_some(),
                    answered.is_some(),
                    &screen.rows,
                )
            }
            None => {
                // The wall is over once the worker worked (the policy's own
                // rule, [`TurnEndState::observe`]): the next is a new reset.
                if worked.is_some() {
                    self.wall_reset = None;
                    self.reprint_jumps = 0;
                }
                None
            }
        };
        // ... and written down once its reset is placed, for a loop that
        // follows this one.
        self.persist_wall(screen.seq);
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
            self.composer_draft(reading.composer.as_ref(), screen),
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
        // This session's share of the account's herd.
        let jitter = match (&reading.wall, self.sid.as_deref()) {
            (Some(w), Some(sid)) if resumes_at_reset(w.kind) => resume_jitter(
                sid,
                self.wall_reset.as_ref().and_then(|(_, u)| *u).unwrap_or(0),
                self.turn_end.timing.reset_jitter,
            ),
            _ => Duration::ZERO,
        };
        // THE MEMORY CEILING ACTED ON: the host's measure of the agent's
        // memory past `[harness] memory_ceiling` WHILE the Mac is in trouble
        // and this agent holds the most (`supervise/memory.rs`,
        // `MemoryOver::acts`), remembered for the wait's edge
        // ([`Self::memory_edge`]). Past the ceiling with memory to spare, or
        // behind a larger agent, is a warning only ([`Self::memory_warn`]).
        self.memory_seen = host
            .and_then(|h| h.memory())
            .and_then(|m| super::super::memory::judge(opts.policy.memory_ceiling, &m))
            .filter(super::super::memory::MemoryOver::acts);
        // At an idle point, or one the turn ended on a wall of its own (a
        // usage or spend limit, a lost login, an API error waited out), since
        // a weekly limit or a lost login may stand for days: the restart comes
        // first; the wall, still there on the resumed agent's screen or met
        // again at its next turn, is then decided by its own rule. The banner
        // is the memory wall already.
        let measured = self.memory_seen.filter(|_| {
            r.wall != Some(aterm_phase::WallKind::Memory)
                && matches!(r.phase, Phase::Idle | Phase::Limited { .. })
        });
        self.memory_wall = measured.is_some();
        // …in place of the screen's own wall: its act is the memory's now,
        // handed back when the measure falls under the ceiling again.
        self.memory_over_wall =
            self.memory_wall && (r.wall.is_some() || matches!(r.phase, Phase::Limited { .. }));
        let mut reading = TurnEndReading {
            jitter,
            reach: self.reach_seen.unwrap_or_default(),
            person: self.person_ago(Instant::now()),
            limits: seen.limits,
            sandbox_fell: seen.sandbox_fell,
            upgrade_goal: r.program == aterm_phase::Program::Codex && self.goal_held_by_upgrade(),
            thread_model,
            // Never while a limit episode stands: the upgrade types nothing
            // at a limit, and the wall is the loop's to wait out
            // ([`crate::supervise::IdleHost::limited`]); nor while Codex's
            // save-then-wait switch is open: the session is the switch's.
            upgrading: self.limit.is_none()
                && !self.turn_end.switch_open()
                && host.is_some_and(|h| h.owns_turn_end()),
            restartable: host.is_some_and(|h| h.can_restart()),
            resume: host.and_then(|h| h.resume_command()),
            // The screen's launch card, or the host's record of whose turns
            // the conversation holds (the harness's own are no task).
            taskless: r.taskless || host.is_some_and(|h| h.taskless()),
            ..r
        };
        // …read, at an idle point (a wall's own included, whose kind it takes
        // the place of) as the wall Claude Code's critical-memory banner is:
        // the same restart at the same idle point, or the same escalation
        // where no host makes one, with the measure as its fact. Never at a
        // box or a question: its answer is not held back for a restart that
        // cannot be made there.
        if let Some(over) = measured {
            reading.wall = Some(aterm_phase::WallKind::Memory);
            reading.wall_message = over.fact();
        }
        self.account_tells(&mut reading, screen.seq, opts);
        reading
    }

    /// A DECISION BEGINS ([`Self::turn_end_at_point`], [`Self::turn_end_now`],
    /// a break the switch takes as its point): what the host advised about
    /// the account for the decision before is forgotten, so this one asks
    /// again ([`Self::account_advise`]) — a wait that ran out is how a
    /// session with no account to run on learns that one is usable.
    fn account_round(&mut self) {
        self.account_seen = None;
    }

    /// THE SESSION'S ACCOUNT as every reading of a point carries it
    /// (`docs/DESIGN-claude-accounts-2026-10-01.md` §6), in this order:
    ///
    /// 1. THE TELLS. A move this session's host made and no point has
    ///    answered ([`TurnEndState::account_answer`], read off the reading
    ///    BEFORE it is folded in): served — the first turn asked under the
    ///    new login ran and ended with no wall ([`IdleHost::account_served`])
    ///    — or answered by a usage limit or a lost login
    ///    ([`IdleHost::account_answered`]). Each once a move — and once for
    ///    every loop on the session: the tell is written to the approval
    ///    ledger ([`approvals::account_answer_reason`]), where a loop that
    ///    starts after this one reads it back
    ///    ([`approvals::account_moves`]) and neither tells a bad answer a
    ///    second time nor forgets a move still to be served.
    /// 2. THE ADVICE THIS DECISION WAS GIVEN ([`Self::account_advise`]),
    ///    for the readings of the same point built after it — an act typed,
    ///    a restart — each of which takes the answer the decision got
    ///    ([`Session::account_seen`], keyed by the screen and the point). A
    ///    tell forgets it: the host has struck the account since, and its
    ///    advice is the next one.
    ///
    /// No host, one that keeps no roster (the trait's defaults), or
    /// `[harness] accounts = "off"`: nothing is told and the reading's
    /// advice is `None` — the reading it was before accounts existed.
    fn account_tells(&mut self, reading: &mut TurnEndReading, seq: u64, opts: &SuperviseOpts) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        if opts.policy.accounts == crate::harness::accounts::Mode::Off {
            return;
        }
        if let Some(answer) = self.turn_end.account_answer(reading) {
            let to = self
                .turn_end
                .account_move()
                .map(|m| m.to.clone())
                .unwrap_or_default();
            match answer {
                AccountAnswer::Served => host.account_served(),
                AccountAnswer::Bad(bad) => host.account_answered(bad),
            }
            self.turn_end.account_told(answer);
            self.account_seen = None;
            self.ledger_row(
                RULE_ACCOUNT,
                approvals::Outcome::Waiting,
                &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
                &approvals::account_answer_reason(&to, answer),
                seq,
            );
        }
        if let Some((at, asked, advice)) = &self.account_seen
            && *at == seq
            && self.turn_end.account_point(reading, &opts.policy) == Some(*asked)
        {
            reading.account.clone_from(advice);
        }
    }

    /// THE HOST'S ADVICE about the session's account ([`IdleHost::account`]),
    /// asked AFTER the point is folded into the policy's memory
    /// ([`TurnEndState::observe`]) — the fold is what ends a latch with its
    /// wall and opens a finished task again — only where it could decide
    /// something ([`TurnEndState::account_point`]), and ONCE A DECISION
    /// ([`Self::account_round`] begins one). The accounts' NEWS goes first
    /// ([`Self::account_news`]): the stamp is read before the host reads its
    /// roster, so a change that lands meanwhile moves it past what is kept
    /// and the wait that follows is decided again ([`Self::account_edge`]).
    /// Nothing under `[harness] accounts = "off"`, and nothing with no host.
    pub(super) fn account_advise(
        &mut self,
        reading: &mut TurnEndReading,
        seq: u64,
        opts: &SuperviseOpts,
    ) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        if opts.policy.accounts == crate::harness::accounts::Mode::Off {
            return;
        }
        self.account_news(host.as_ref());
        let Some(point) = self.turn_end.account_point(reading, &opts.policy) else {
            return;
        };
        let advice = host.account(point);
        self.account_seen = Some((seq, point, advice.clone()));
        reading.account = advice;
    }

    /// THE ACCOUNTS' NEWS, read off the host's stamp
    /// ([`IdleHost::account_stamp`]): `true`, once, when it has moved since
    /// this session last read it — and then what the policy remembers of a
    /// move the host refused is let go ([`TurnEndState::account_news`]), so
    /// a remedy the owner applied (`accounts login` and `check`, `accounts
    /// use`) reaches a tab whose move was refused. A refusal with no news
    /// after it stays as it was. THE FIRST READ IS NO NEWS: the stamp counts
    /// since the host started, and read as news it ended, at the first
    /// decision of every loop that started later, the latch the loop before
    /// left in the ledger ([`TurnEndState::seed_account_unmade`]).
    pub(super) fn account_news(&mut self, host: &dyn IdleHost) -> bool {
        let stamp = host.account_stamp();
        if self
            .account_stamp
            .replace(stamp)
            .is_none_or(|read| read == stamp)
        {
            return false;
        }
        self.turn_end.account_news();
        true
    }

    /// Whether the composer holds a PERSON'S draft, judged where it is DRAWN.
    /// A row of text on the caret row is
    /// Claude's dim suggestion or a draft: the cursor's COLUMN says which
    /// where it can — a program parks it at column 2, after the caret, on an
    /// empty composer, and a typed draft leaves it inside the text — and at
    /// column 2 the `cell` there decides (the placeholder is drawn DIM,
    /// [`crate::supervise::screen::cell_is_dim`]; a typed draft is not, and
    /// neither is one whose caret was moved home: ←, Home, ctrl-a, measured on
    /// Claude Code 2.1.280 and 2.1.281). The cursor's ROW is not trusted: a
    /// window that re-gridded twice in a millisecond leaves an
    /// alternate-screen program's rows one higher than it drew them and its
    /// cursor on the row below the prompt (seen live for hours, through
    /// exactly the idle stretch a wall is), and read by the row, Claude's own
    /// suggestion was a person's draft and the wall's act became a bare
    /// Enter on ghost text instead of `keep going`. A second row of text is a draft. A `cell` the
    /// server does not answer reads as the placeholder where the cursor
    /// stands on the caret row, and as a DRAFT anywhere else:
    /// nothing is typed over text that may be a person's. `composer` is the
    /// region the reader already read off `screen` ([`aterm_phase::Reading::composer`]).
    pub(super) fn composer_draft(
        &mut self,
        composer: Option<&(usize, Vec<String>)>,
        screen: &Screen,
    ) -> bool {
        let Some((caret, lines)) = composer else {
            return false;
        };
        if lines.iter().skip(1).any(|l| !l.is_empty()) {
            return true;
        }
        if lines.first().is_none_or(String::is_empty) {
            return false;
        }
        if screen.cursor_col != 2 {
            return true;
        }
        let row = (screen.first + caret).to_string();
        match self.call(&["cell", &row, "2"]) {
            Ok(r) if r.ok() => !crate::supervise::screen::cell_is_dim(&r.stdout),
            _ => screen.cursor_index() != Some(*caret),
        }
    }

    /// The decision at a NEW point: the point folded into the policy's
    /// memory ([`TurnEndState::observe`]) — once, here — the host asked for
    /// its advice about the account on the memory so folded
    /// ([`Self::account_advise`]), then [`decide_turn_end`]; and when that is
    /// an act, decided again on a fresh `status` ([`Self::decide_to_act`]). A
    /// loop watching behind another supervisor's claim decides nothing: that
    /// one answers the session.
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
        self.account_round();
        let mut r = self.turn_end_reading(&point.screen, worked, opts);
        // Decided while the host owns the turn ends: decided again once it
        // owns nothing ([`Self::host_held`]).
        self.host_held = r.upgrading;
        let now = Instant::now();
        self.person_for_the_hold(&mut r, now)?;
        self.turn_end.observe(&r, now);
        self.account_advise(&mut r, point.screen.seq, opts);
        self.decide_to_act(r, opts, now)
    }

    /// A held switch whose footer shows another model than its own: whose the
    /// change is (a person's `/model` since the hold began releases the
    /// switch, and is never driven back, [`TurnEndState::observe`]) is judged
    /// on a person's last keystroke read fresh (`status`), before the point is
    /// folded in.
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

    /// [`decide_turn_end`] on `r`; when it says to type, the session's program
    /// and a person's last keystroke are read fresh (`status`,
    /// [`Self::foreground_program`]) and the point decided again on them, so
    /// a status read is paid only where there is something to type.
    ///
    /// A question asked in prose ([`TurnEndReading::asked`]) is decided by the
    /// session's priority word where it has one ([`Self::prose_policy`]):
    /// `recommended`, `long-term` or `short-term` answers it by that priority,
    /// over the table's `question_priority` and over its `answer_questions =
    /// false` (contract decision 7: the prose path and the dialog path resolve
    /// the same word). The word is read (one bare `meta`) only where it can
    /// decide: a point the table would answer, or hold only because the owner
    /// switched the answers off ([`ANSWERS_OFF`]); never at a wait, a cap or
    /// a wall. The session's `ask`, and a word this build does not know, hand
    /// over its dialogs only (`aterm drive answer` answers them, `meta unset
    /// questions` hands them back): a question in prose has neither, so under
    /// those words it is the table's.
    fn decide_to_act(
        &mut self,
        mut r: TurnEndReading,
        opts: &SuperviseOpts,
        now: Instant,
    ) -> Result<TurnEndAction, Fail> {
        let mut action = decide_turn_end(&self.turn_end, &r, &opts.policy, now);
        let mut cfg = std::borrow::Cow::Borrowed(&opts.policy);
        if r.asked().is_some()
            && session_word_can_decide(&action)
            && let Some(policy) = self.prose_policy(opts)?
        {
            cfg = std::borrow::Cow::Owned(policy);
            action = decide_turn_end(&self.turn_end, &r, &cfg, now);
        }
        // The exceptions engine reads the same policy the loop decides under.
        self.exception_now = exception_for(&self.turn_end, &r, &cfg, now);
        if action.rule_id().is_none() {
            return Ok(action);
        }
        self.program = self.foreground_program()?;
        r.person = self.person_ago(now);
        Ok(decide_turn_end(&self.turn_end, &r, &cfg, now))
    }

    /// The policy a question asked in prose is decided under when the
    /// session's own `questions` word is a priority word
    /// ([`Self::question_policy`], the dialog's one resolver; not where the
    /// loop gives a session no say, `session_questions`): the table's,
    /// answering, by that word's priority. `None` (no session word, `ask`, or
    /// an unknown word): the table decides.
    fn prose_policy(&mut self, opts: &SuperviseOpts) -> Result<Option<SupervisorConfig>, Fail> {
        if !opts.policy.session_questions {
            return Ok(None);
        }
        let policy = self.question_policy(opts)?;
        Ok(match policy.answer {
            Some(word) if policy.by_session => {
                let mut cfg = opts.policy.clone();
                cfg.answer_questions = true;
                cfg.question_priority = word;
                Some(cfg)
            }
            _ => None,
        })
    }

    /// The policy's `WaitUntil` ran out on the point still showing: read the
    /// screen again, and when it still shows the same point (its review key)
    /// fold it in again with no work ([`TurnEndState::observe`]: the same
    /// point read again changes nothing, except an act whose point has shown
    /// past [`TurnEndTiming::take_within`] with no busy read, which is judged
    /// now), decide again, and carry it out; an escalation is the point's
    /// ([`Self::escalate_point`]). A screen that shows another point is left to
    /// the next look.
    pub(super) fn turn_end_now(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.turn_end_due = None;
        // The wait the host was told (an API wall's next try) is over: the
        // point is decided again now, and the try is typed below.
        self.end_told_wait(opts);
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        // An act held for a person: the next look judges the point again.
        if std::mem::take(&mut self.held) {
            self.rejudge = true;
            return Ok(());
        }
        let screen = self.screen()?;
        let mut turn = self.turn_of(screen);
        if self.review_key(&turn) != self.review_key(seen) {
            return Ok(());
        }
        self.account_round();
        let mut r = self.turn_end_reading(&turn.screen, None, opts);
        self.refused_limit(&mut turn);
        self.host_held = r.upgrading;
        let now = Instant::now();
        self.person_for_the_hold(&mut r, now)?;
        self.turn_end.observe(&r, now);
        self.account_advise(&mut r, turn.screen.seq, opts);
        // A probe the roster says is due at a point decided AGAIN is not
        // run — the turn behind it was not read live by this look — and says
        // so, unless the point has had its line ([`Self::probe_shut`]).
        self.probe_shut(&turn, ProbeGate::NotNew, None, review)?;
        let action = self.decide_to_act(r, opts, now)?;
        if let TurnEndAction::Escalate { reason } = &action
            && self.escalates_here(&turn.phase)
        {
            self.escalate_point(&turn, Some(reason), review)?;
            // The memory wall's escalation took the point's own act: handed
            // back when the measure is no longer acted on
            // ([`Self::memory_edge`]).
            if self.memory_wall {
                self.mark_memory_badge();
            }
        }
        self.turn_end_execute(&turn, action, opts, allow, review)
    }

    /// Whether the policy's escalation at a point of `phase` is the point's
    /// own ([`Self::escalate_point`]): at an idle point and a question, and at
    /// a limit notice only when the reading took it as the memory wall
    /// ([`Self::memory_wall`]). A limit's own escalation is its episode's
    /// (`limit_after`), which then raises nothing over the memory's
    /// (`handled`).
    pub(super) fn escalates_here(&self, phase: &Phase) -> bool {
        match phase {
            Phase::Idle | Phase::Question => true,
            Phase::Limited { .. } => self.memory_wall,
            _ => false,
        }
    }

    /// Carry out what the policy decided at `point` (module docs). An act the
    /// server took is recorded in the policy's memory
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
        // A wall the transcript did not confirm, journaled once.
        self.say_wall_refused(review);
        // The point a person alone can answer, said once per change.
        self.sync_exception(seq, review);
        // What Codex's save-then-wait switch and its sandbox check left to
        // ledger and to say, before the point's act.
        self.wind_said(point, review)?;
        self.say_sandbox_fall(point, review)?;
        // The task ended at the done check: said once, to the journal alone
        // — nothing is typed from here until someone else's turn.
        let done = self.turn_end.task_done();
        if done && !self.task_done_said {
            if self.turn_end.finished_by_worker() {
                review.note(&format!(
                    "DONE seq={seq} rule={RULE_FINISHED} the worker reported {FINISHED}: its task \
                     is finished; nothing more is typed until a person or an orchestrator types"
                ));
            } else {
                review.note(&format!(
                    "DONE seq={seq} rule={RULE_DONE_CHECK} the worker said it is done after the \
                     done check: nothing more is typed until a person or an orchestrator types"
                ));
            }
        }
        self.task_done_said = done;
        // A restart being waited for is one wall's; whatever else the point
        // decides ends the run of tries, and the next restart starts its
        // pauses over.
        if !matches!(action, TurnEndAction::Restart { .. }) {
            self.restart_waits = 0;
        }
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
                let said = self.turn_end_due.as_ref().is_some_and(|d| d.at == *until);
                if !said {
                    review.note(&format!(
                        "WAITING seq={seq} until={} {why}",
                        self.stamp_in(*until)
                    ));
                }
                self.set_due(*until, why.clone());
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
        // holds paused for its move: the goal is the upgrade's, and the switch
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
        // The draft this act would send in its place is the accounts probe's
        // own `/usage`, left typed: its Enter is the probe's, and no act.
        if matches!(action, TurnEndAction::Submit { .. }) && self.probe_left_stands(&point.screen) {
            return self.probe_left_enter(point, review);
        }
        let typed = match &action {
            TurnEndAction::Accept { .. } => self.accept_suggestion(point, &text, rule, review)?,
            TurnEndAction::Submit { .. } => self.submit_draft(point, &text, rule, review)?,
            _ => self.type_text(point, &text, rule, review)?,
        };
        if !typed {
            return Ok(());
        }
        let r = self.turn_end_reread(&point.screen, opts);
        self.turn_end.acted(&action, &r, Instant::now());
        // The switch's `/goal resume` at its reset over the live upgrade's
        // pause: the upgrade's resume, made by the switch, on its record, so
        // the upgrade verifies it and never makes its own.
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

    /// THE POINT ONLY A PERSON CAN ANSWER, said once per change
    /// ([`exception_for`]): `EXCEPTION seq=<n> kind=<word> when=<later|now>
    /// <why> => <action>` to the journal and the host's
    /// [`IdleHost::exception`] when one appears or its words change, and, when
    /// the point no longer holds it, its end ([`Self::exception_ended`]).
    /// Says nothing where the exception stands as told, and raises nothing
    /// (no attention, no badge, no mail): a lost login's own escalation is
    /// unchanged, and the rest is the host's digest or the journal.
    fn sync_exception(&mut self, seq: u64, review: &mut dyn Review) {
        // A point is the screen moving on: a stall no reader named is over.
        self.stuck_over(seq, "the screen reached a point", review);
        let Some(now) = self.exception_now.clone() else {
            self.exception_ended(seq, None, review);
            return;
        };
        match &self.exception_open {
            Some((open, _)) if *open == now => return,
            Some((open, _)) if open.kind != now.kind => {
                self.exception_ended(seq, Some("another takes its place"), review);
            }
            _ => {}
        }
        let since = self
            .exception_open
            .as_ref()
            .map_or_else(Instant::now, |(_, t)| *t);
        review.note(&format!(
            "EXCEPTION seq={seq} kind={} when={} {} => {}",
            now.kind.word(),
            now.when.word(),
            now.why,
            now.action
        ));
        if let Some(host) = &self.stall_host {
            host.exception(Some(&now));
        }
        self.exception_open = Some((now, since));
    }

    /// The exception told and standing is over: `CLEARED seq=<n>
    /// exception=<word> after=<s>s <what cleared it>` to the journal, and the
    /// host's [`IdleHost::exception`] told `None`. Nothing stood: nothing is
    /// said. `cause` names the end where the exception's own words for it
    /// (`clears`) would not be true — the loop ending, a replacement.
    pub(super) fn exception_ended(
        &mut self,
        seq: u64,
        cause: Option<&str>,
        review: &mut dyn Review,
    ) {
        self.exception_now = None;
        let Some((open, since)) = self.exception_open.take() else {
            return;
        };
        review.note(&format!(
            "CLEARED seq={seq} exception={} after={}s {}",
            open.kind.word(),
            since.elapsed().as_secs(),
            cause.unwrap_or(open.clears)
        ));
        if let Some(host) = &self.stall_host {
            host.exception(None);
        }
    }

    /// THE RESTART the policy asked for ([`TurnEndAction::Restart`]: the
    /// memory banner, D3; a model bucket's fallback and its reset, D7): the
    /// session's host's ([`IdleHost::restart`]), journaled `HOST seq=<n>
    /// restart:<why> step=<word>`, and what the loop does with its word. The
    /// agent relaunched (`adopted`) is the loop's again at once (the host asks
    /// for the next idle point to carry it on), said `RESTARTED seq=<n>
    /// rule=<id> <what>`, remembered by the policy
    /// ([`TurnEndState::restarted`]: a bucket's switch, undone at its reset)
    /// and, for a model, ledgered as `relaunch --model <m>`, the row a later
    /// loop reads the open switch from ([`approvals::open_model_switch`]). One
    /// that must wait (`wait:…`, `busy:another-sweep`) is decided again after
    /// a pause that grows ([`TurnEndTiming::restart_backoff`]), never
    /// escalated for the wait. One that is never made (no host restarts here,
    /// a launch that cannot be carried, a signal refused) gets the point's
    /// `otherwise`: the memory banner's escalation (naming why), a bucket's
    /// reset waited out, the continuation a reset owed. Nothing while the
    /// worker's stall is in hand (`stall.rs`).
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
        // The measure the point was decided on, if the memory wall was the
        // host's measure rather than the banner (read again below).
        let measured = self.memory_seen;
        // …and whether it stood in for the screen's own wall (read again
        // below, perhaps under the ceiling by then), or for any idle point's
        // own act: an escalation in the restart's place is the memory's,
        // handed back when the measure is no longer acted on.
        let over_wall = self.memory_over_wall && matches!(why, Restart::Memory);
        let took = self.memory_wall && matches!(why, Restart::Memory);
        // THE ROW SAYS THE RESTART BEFORE IT IS MADE: the point's own reading
        // ([`Self::turn_end_reading`]) may be the first to find the measure
        // acted on — the host's cached measure expired since the turn's last
        // busy read, which said `a warning only` (or nothing, in a loop that
        // began at the point) — and no wait step comes between it and this
        // ask to say the new verdict ([`Self::memory_warn`] is said from the
        // waits). A refusal below says the person's; one standing from an
        // earlier point keeps it until a restart is made (said below).
        if let (Restart::Memory, Some(over)) = (why, measured) {
            self.memory_warn(&point.screen, over, opts, review);
        }
        let step = opts.idle_host.as_ref().and_then(|h| h.restart(why));
        if let Some(step) = &step
            && restart_note_due(self.restart_waits)
        {
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
                // A restart made: a refusal before it no longer stands, and
                // the next warning names aterm's restart again
                // ([`Self::memory_refused`]).
                self.memory_refused = false;
                let r = self.turn_end_reread(&point.screen, opts);
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
                    Restart::Memory => match measured {
                        Some(over) => format!("the agent, for its memory: {}", over.fact()),
                        None => "the agent, for its critical-memory banner".to_string(),
                    },
                    Restart::Model { to } => format!("relaunch --model {to}"),
                    Restart::ModelBack { to } => {
                        format!(
                            "relaunch --model {}",
                            to.as_deref().unwrap_or("(its launch's)")
                        )
                    }
                    // The LABEL, and nothing else of the account, in this
                    // loop's journal and its approval ledger. (The UPGRADE's
                    // ledger is the relaunch's own record and keeps the line
                    // it typed as its `detail`, which names the login
                    // directory beside the `cd` it always held: a path,
                    // never a credential.)
                    Restart::Account { to, .. } => {
                        format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND)
                    }
                };
                // The wall the move left is the OLD account's: what the
                // loop remembers of it (the reading above read it off the
                // point's screen once more) is not the new process's, nor is
                // the advice asked for that point — nor the LIMIT EPISODE,
                // which held every step of the host's back until that
                // account's reset plus an hour ([`Guard::LimitEpisode`]): the
                // relaunched agent's carry-on with them, so a move resumed
                // later than no roster would have.
                if matches!(why, Restart::Account { .. }) {
                    self.forget_wall(seq);
                    self.wall_reset = None;
                    self.reprint_jumps = 0;
                    self.account_seen = None;
                    self.retry = None;
                    self.close_episode(
                        seq,
                        "relaunched under another account, off the limited one",
                        review,
                    )?;
                }
                let switch = self
                    .turn_end
                    .model_switch()
                    .filter(|_| matches!(why, Restart::Model { .. }))
                    .cloned();
                let back = match (&switch, why) {
                    (Some(m), _) => format!(
                        " (from {}, {}; session-only)",
                        m.from.as_deref().unwrap_or("the launch's model"),
                        // No reset named: no return is scheduled, and none is
                        // guessed.
                        m.back_at.map_or_else(
                            || "no return scheduled: the notice names no reset".to_string(),
                            |at| format!("back at {}", self.stamp_in(at))
                        )
                    ),
                    (
                        None,
                        Restart::Account {
                            at: AccountAt::Wall,
                            ..
                        },
                    ) => " (from the limit its turn ended on; the same conversation)".to_string(),
                    (
                        None,
                        Restart::Account {
                            at: AccountAt::Ahead,
                            ..
                        },
                    ) => " (at a turn end; the same conversation)".to_string(),
                    (None, _) => String::new(),
                };
                review.say(&format!("RESTARTED seq={seq} rule={rule} {what}{back}"))?;
                // A restart made over a refusal that stood (the row the
                // person's until here) names aterm's at once
                // ([`Self::memory_say`]).
                if let (Restart::Memory, Some(over)) = (why, measured) {
                    self.memory_say(&point.screen, over, true, opts, review);
                }
                if !matches!(why, Restart::Memory) {
                    // A switch's row carries how to undo it, for a loop that
                    // starts before its reset.
                    let reason = match (&switch, why) {
                        (Some(m), _) => approvals::model_switch_reason(
                            m.from.as_deref(),
                            m.back_at.map(|at| self.unix_of(at)),
                        ),
                        // The move's row: a loop that starts after it counts
                        // it and answers it ([`approvals::account_moves`]) —
                        // and, a `typed` row, it ends the wall the earlier
                        // rows waited on ([`approvals::open_wall`]), which is
                        // right: the session left that wall's account.
                        (None, Restart::Account { to, at }) => {
                            approvals::account_switch_reason(to, *at)
                        }
                        (None, _) => "the turn-end policy".to_string(),
                    };
                    self.ledger_row(rule, approvals::Outcome::Typed, &what, &reason, seq);
                }
                Ok(())
            }
            // The RETURN to the
            // bucket's model is optional — the session works on the fallback —
            // so a return the host cannot make yet (`wait:changed` while the
            // agent's own background work runs: the relaunch would end it)
            // never holds the point back. The point's own act goes ahead
            // (`otherwise`: the continuation), the switch stays open, and the
            // return is asked again at the next idle point after the reset —
            // the first quiet one makes it. Waiting for it, as for a
            // fallback, typed nothing at any point of a tab that always has a
            // job running (69 asks in 1.5 s, no continuation, in a replay
            // of the policy).
            //
            // A MOVE TO ANOTHER ACCOUNT AHEAD OF A WALL is optional in the
            // same way — the session works where it is — and deferred the
            // same way (the accounts design, §6): a tab that always has a
            // job running would otherwise never be continued again.
            (Some("wait"), Some(step)) | (Some("busy"), Some(step))
                if matches!(
                    why,
                    Restart::ModelBack { .. }
                        | Restart::Account {
                            at: AccountAt::Ahead,
                            ..
                        }
                ) && (word == Some("wait") || step == "busy:another-sweep") =>
            {
                review.note(&format!(
                    "DEFERRED seq={seq} restart:{} waits for a quiet moment ({step}); the point goes on",
                    why.word()
                ));
                self.turn_end_execute(point, otherwise, opts, allow, review)
            }
            // A FALLBACK, a move to another account AT A WALL, and the memory
            // banner's restart, the host cannot make
            // yet is asked again on [`TurnEndTiming::restart_backoff`], the
            // last pause for every try after it — cut short at the bucket's
            // reset ([`Self::restart_later`]), where the continuation is
            // tried first ([`wall_action`]): nothing is given up for a
            // count, and nothing is typed blind at a wall a relaunch on the
            // fallback answers as soon as the agent's own jobs end.
            (Some("wait"), Some(step)) => {
                let r = self.turn_end_reread(&point.screen, opts);
                let due = r
                    .reset_at
                    .map(|at| at + self.turn_end.timing.grace_for(r.program) + r.jitter);
                self.restart_later(seq, why, step, due, review)
            }
            (Some("busy"), Some(step)) if step == "busy:another-sweep" => {
                self.restart_later(seq, why, step, None, review)
            }
            _ => {
                self.restart_waits = 0;
                let r = self.turn_end_reread(&point.screen, opts);
                // The memory's `otherwise` still replaces the wall's own act
                // even when this read is no longer acted on: the next measure
                // not acted on hands the point back ([`Self::memory_edge`]).
                self.memory_over_wall |= over_wall;
                self.turn_end.restarted(why, false, &r, Instant::now());
                // An account move that could not be made: written down, so a
                // loop that starts at the same wall does not ask again
                // ([`approvals::account_moves`]). `skipped`, never `typed`:
                // the wall the earlier rows waited on still stands.
                if let Restart::Account { to, at } = why {
                    self.ledger_row(
                        rule,
                        approvals::Outcome::Skipped,
                        &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
                        &approvals::account_unmade_reason(to, *at, step.as_deref()),
                        seq,
                    );
                }
                // THE WARNING AGREES WITH THE BADGE: nothing asks the host
                // again while this point stands, and the next point's ask is
                // mostly refused the same (a launch-bound refusal stands for
                // the agent's life; a `busy:` or a relaunch switched off live
                // may not), so the row's verdict is the person's restart until
                // the next point asks again or a restart is made, said again
                // at once ([`Self::memory_refused`],
                // [`Self::memory_warn`]) — never `aterm restarts it` beside
                // `the restart could not be made`.
                if matches!(why, Restart::Memory) {
                    self.memory_refused = true;
                    if let Some(over) = self.memory_measure(opts) {
                        self.memory_warn(&point.screen, over, opts, review);
                    }
                }
                match otherwise {
                    TurnEndAction::Escalate { reason } => {
                        let reason = match &step {
                            Some(step) => {
                                format!("the restart could not be made ({step}): {reason}")
                            }
                            None => reason,
                        };
                        self.escalate_point(point, Some(&reason), review)?;
                        if took {
                            self.mark_memory_badge();
                        }
                        Ok(())
                    }
                    other => self.turn_end_execute(point, other, opts, allow, review),
                }
            }
        }
    }

    /// The Mac woke at `wall_now` (unix seconds) from a sleep the wait went
    /// through. A deadline the sleep carried past ([`Due::unix_ms`], its wall
    /// time) would fall at once ([`Self::due_at`]); it is decided again
    /// [`WAKE_SETTLE`] (and the session's share of [`WAKE_SPREAD`]) from now
    /// instead, on the reset the unix clock reads afresh
    /// ([`Self::wall_reset_at`]). A deadline still ahead falls at its wall time
    /// as it stands. ONLY where a wait stands: a point with nothing waited on
    /// (an escalated one, a question) is decided again by nothing here, or its
    /// ask would be posted twice ([`Self::reach_edge`]'s rule).
    pub(super) fn woke(&mut self, seq: u64, wall_now: i64, review: &mut dyn Review) {
        if self
            .turn_end_due
            .as_ref()
            .is_none_or(|d| d.unix_ms > wall_now.saturating_mul(1000))
        {
            return;
        }
        let share = self.sid.as_deref().map_or(Duration::ZERO, |sid| {
            resume_jitter(sid, wall_now.div_euclid(60), WAKE_SPREAD)
        });
        let at = Instant::now() + WAKE_SETTLE + share;
        review.note(&format!(
            "WAITING seq={seq} until={} the Mac woke: deciding again",
            self.stamp_in(at)
        ));
        self.set_due(at, "the Mac woke");
    }

    /// A restart that must wait: decided again after the next pause of
    /// [`TurnEndTiming::restart_backoff`], `WAITING … until=<t>
    /// restart:<why> waits: <step>`.
    fn restart_later(
        &mut self,
        seq: u64,
        why: &Restart,
        step: &str,
        cut_at: Option<Instant>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let pause = crate::supervise::ladder::Ladder(&self.turn_end.timing.restart_backoff)
            .step(usize::try_from(self.restart_waits).unwrap_or(usize::MAX));
        self.restart_waits = self.restart_waits.saturating_add(1);
        let now = Instant::now();
        let mut until = now + pause;
        // The bucket's reset, when it comes before the next try: decided
        // then, the continuation first.
        if let Some(at) = cut_at.filter(|&at| at > now) {
            until = until.min(at);
        }
        let why = format!("restart:{} waits: {step}", why.word());
        if restart_note_due(self.restart_waits.saturating_sub(1)) {
            review.note(&format!(
                "WAITING seq={seq} until={} {why}",
                self.stamp_in(until)
            ));
        }
        self.set_due(until, why);
        Ok(())
    }

    /// Whether the session's foreground program, read fresh for this decision
    /// ([`Self::turn_end_at_point`], [`Self::turn_end_now`]), is an agent this
    /// policy types into: one whose reader is supervisable
    /// ([`aterm_phase::Program::supervisable`]: Claude Code, Codex), never a
    /// shell or a program nothing reads. Otherwise journaled `SKIPPED`, nothing
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
        // The mark the composer is drawn with on this screen: Codex 0.158.0
        // draws its input line `»` where 0.157 drew `›`, and a guard spelled
        // with the other would never be pressed under.
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
        // typed (`reason=guard`), in the composer or wherever the cursor was.
        // Said, never left for the next look to read as a person's draft.
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

    /// A fenced write the server skipped on the unchanged screen (a bare `OK
    /// skipped`: the generation held and the guard matched no row): nothing
    /// written, and a retry would miss the same way, so it is journaled
    /// `SKIPPED … the guard matched no row …` and escalated.
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
        self.set_due(Instant::now() + REDECIDE_AFTER_SKIP, why);
    }

    /// Text this policy wrote and did not submit: escalated (the worker's
    /// `attention`, one ask), the badge cleared when the worker works. The
    /// accounts probe's own `/usage` is the one text of the loop's this is
    /// NOT called for: left typed, it is a record ([`Self::probe_type`]).
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

    /// `text` typed and submitted: [`Self::type_fenced`] where the host fences
    /// `send`, else [`Self::type_guarded`] (module docs). A program that takes
    /// an Enter right behind typed text as a newline (Codex's paste guard,
    /// [`ScreenReader::paste_guard`]) is written fenced too, its Enter held
    /// until the echo has been still [`PASTE_GUARD_SETTLE_MS`], so text never
    /// lands in a shell an agent exited to.
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

    /// Whether the host's `send` takes the `if-gen=` fence (`help send` names
    /// it; an older `send` types a leading `if-gen=` as text, so it is never
    /// sent unprobed).
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

    /// The fenced write (module docs): `None` when the host or the read cannot
    /// fence it (no generation on the read, no composer, `key`/`send` without
    /// `if-gen=`), and the caller falls back to [`Self::type_guarded`]; else
    /// whether the text was submitted.
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
            // the point is the owner's, said.
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

    /// The draft standing in the composer submitted as it is (module docs):
    /// Enter, fenced on the judged read's generation and guarded on its caret
    /// row. `text` is the draft as read, for what is said. A host or a read
    /// without the fence gets nothing pressed: the draft is escalated.
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
        let Some((r, args)) =
            self.press_fenced(&rows[caret], "enter", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped_changed() {
            // A keystroke since the read: decided again on the screen as it
            // is now.
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the screen moved before Enter; the draft stays"
            ));
            self.redecide_soon("the screen moved before the draft's Enter");
            return Ok(false);
        }
        if r.skipped() {
            // The judged screen, and its caret row matched no row of it
            // ([`Self::type_fenced`]'s bare skip).
            self.not_written(point, text, rule, &args, review)?;
            return Ok(false);
        }
        Ok(true)
    }

    /// `key if-gen=<generation> if=<row's guard> <key>`, the fenced press
    /// [`Self::submit_draft`] and [`Self::accept_suggestion`] make, with a
    /// refusal answered as both answer one: a lost connection is the error; a
    /// hold is parked on, a busy server backed off from, an unconfirmed press
    /// noted, each `None`, nothing written. `Some` is the server's `OK …`
    /// (pressed or skipped) and the arguments as sent.
    fn press_fenced(
        &mut self,
        row: &str,
        key: &str,
        generation: &str,
        (seq, rule): (u64, &str),
        review: &mut dyn Review,
    ) -> Result<Option<(CtlReply, String)>, Fail> {
        let guard = super::super::policy::row_guard(row);
        let args = super::super::policy::key_args(&guard, key, Some(generation));
        let mut words: Vec<&str> = vec!["key"];
        words.extend(args.split(' '));
        let r = self.call(&words)?;
        match self.refused_press(&r, &args)? {
            None => return Ok(Some((r, args))),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.park_on_hold(&why, seq, review)?,
            Some(Pressing::Refused { why }) => self.back_off(&why, seq, None, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => {
                review.note(&format!("SKIPPED seq={seq} rule={rule} {why}"));
            }
            Some(_) => {}
        }
        Ok(None)
    }

    /// The worker's own suggestion accepted (module docs): `right`, fenced on
    /// the judged read's generation and guarded on the suggestion's row; the
    /// screen read once it has settled; then, when it shows the composer
    /// holding exactly the suggestion, typed now (the cursor off the
    /// placeholder's column), Enter, fenced on that read and guarded on that
    /// row. A host or a read without the generation fence types the
    /// suggestion's words instead ([`Self::type_guarded`]), as does a
    /// suggestion the accept key did not fill.
    ///
    /// A READ TORN MID-REDRAW is not judged. The worker clears its composer
    /// and draws it again as it takes the key; a read between the two (the
    /// screen still moving when [`SETTLE_CAP`] ran out) shows no caret row at
    /// all. That read returned here with the suggestion already FILLED —
    /// nothing pressed, nothing journaled, nothing escalated — and the fill
    /// waited out the draft path's grace, minutes on. It is read once more
    /// after one more settle — a second bounded read on the
    /// [`REACTION_WAIT`] design, not a loop — and a
    /// composer still missing then is journaled `SKIPPED` and nothing more is
    /// pressed (no read showed the fill, so none is claimed).
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
        let Some((r, args)) =
            self.press_fenced(&rows[caret], "right", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
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
        let mut now = self.screen()?;
        if caret_row(self.reader(&now.rows), &now.rows).is_none() {
            // Torn mid-redraw (the doc): one more settle, one more read.
            self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
            now = self.screen()?;
        }
        let reader = self.reader(&now.rows);
        let shows = caret_text(reader, &now.rows).as_deref() == Some(text);
        let filled = shows && typed_draft(reader, &now) && reader.prompt(&now.rows).is_none();
        let (generation, caret) = match (now.generation.clone(), caret_row(reader, &now.rows)) {
            (Some(generation), Some(caret)) => (generation, caret),
            (_, None) => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} no composer after the accept key on two \
                     settled reads; nothing more is pressed"
                ));
                return Ok(false);
            }
            (None, Some(_)) => {
                review.note(&format!(
                    "SKIPPED seq={seq} rule={rule} the read after the accept key carries no \
                     generation to fence Enter on; nothing more is pressed"
                ));
                return Ok(false);
            }
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
        let Some((r, _)) =
            self.press_fenced(&now.rows[caret], "enter", &generation, (seq, rule), review)?
        else {
            return Ok(false);
        };
        if r.skipped() {
            review.note(&format!(
                "SKIPPED seq={seq} rule={rule} the composer moved before Enter; the suggestion \
                 stays filled"
            ));
            return Ok(false);
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// The accounts probe (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §3)
// ---------------------------------------------------------------------------

/// What the host is told, once, when the panel's Escape has failed
/// [`PROBE_STUCK_AT`] times ([`IdleHost::inform`]: a record, no badge): the
/// state on its first line, the one action that ends it on the second.
const PANEL_STUCK: &str = "The /usage panel did not close\nPress Esc in this tab.";

/// What the host is told, once, when the probe's `/usage` was written into
/// the composer and its Enter was not ([`IdleHost::inform`]: a record — no
/// badge, no attention, no ask: the probe's own words left typed are
/// no state a person is needed for NOW, so they are taken out of the
/// escalation every other typed act's leftover gets,
/// [`Session::left_typed`]). The state on its first line; beneath it, what
/// becomes of the text: sent where the policy sends a draft in an act's
/// place ([`Session::probe_left_enter`]); at a point with no act, standing
/// until a person sends or clears it — the loop types nothing more into a
/// composer a person may be at.
const USAGE_LEFT: &str = "/usage was typed and not sent\nThe supervisor's own check of the \
                          account: it is sent with the session's next act, or stands until you \
                          send or clear it; its panel is closed either way.";

/// How many reads in a row must say the same thing of the open panel before
/// the probe acts on it: two `Settled` reads with equal windows are a sample,
/// two folded ones a fold, two that are not the Usage tab another panel. One
/// read alone is a frame that may be half drawn.
const PROBE_READS: u32 = 2;

/// How long the screen must hold still before the probe's next read of it
/// ([`Session::probe_pace`]), in the server's `await idle` milliseconds.
const PROBE_STILL_MS: &str = "50";

/// The host told what a claimed probe read ([`IdleHost::account_probed`]),
/// AT MOST ONCE however the step ends: the first word is the one that
/// counts, and [`Session::usage_probe`] says [`ProbeMiss::Lost`] for a step
/// that ended on an error with nothing told.
struct Telling<'h> {
    host: &'h dyn IdleHost,
    told: bool,
}

impl Telling<'_> {
    fn tell(&mut self, read: Result<&Sample, ProbeMiss>) {
        if !std::mem::replace(&mut self.told, true) {
            self.host.account_probed(read);
        }
    }
}

/// What the probe's fenced write did ([`Session::probe_type`]).
enum ProbeTyped {
    /// `/usage` is in the composer and its Enter was written: `at` is the
    /// server's stamp on the Enter, `human` the person count it was fenced
    /// on — a read that carries another saw a person key since.
    Entered { at: u64, human: u64 },
    /// It was not submitted: what the host is told, why (the journal's
    /// words), and what the look does next. `left`: the text WAS written and
    /// stands in the composer — a record for a person ([`USAGE_LEFT`]),
    /// never an escalation.
    Not {
        miss: ProbeMiss,
        why: String,
        then: Probed,
        left: bool,
    },
}

/// A percentage as the journal's `PROBED` line carries it: `7%`, or `?%`
/// for a figure nobody read.
fn pct_word(pct: Option<u32>) -> String {
    pct.map_or_else(|| "?%".to_string(), |p| format!("{p}%"))
}

impl<C: Ctl> Session<'_, C> {
    /// ONE `/usage` PROBE, AS A STEP OF THE LOOP
    /// (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §3; the model is
    /// aterm-spec's `supervisor_usage_probe_model`): where the host claims
    /// one for this point, type `/usage`, read the Settings panel it opens
    /// until its Usage tab reads SETTLED, hand the host the sample, and close
    /// the panel with an Escape. It is a step between the host's own
    /// ([`Self::host_steps_here`]) and the point's act
    /// ([`Self::turn_end_execute`]), and NO ACT OF THE TURN-END POLICY'S:
    /// [`TurnEndState::acted`] is never called for it, no panel screen is
    /// folded into the policy's memory, and its ledger row is left out of the
    /// typing budget ([`approvals::typed_at`]).
    ///
    /// WHAT WAS MEASURED, and what is only read. MEASURED on Claude Code
    /// 2.1.287, 2026-10-01 (a private headless aterm, six instances, nine
    /// opens, the fullscreen renderer at 150x50, 129x47, 110x32, 60x50 and
    /// 150x80): Enter after `/usage` ran `usage` every time (the typeahead
    /// lists it first, `/usage-credits` second); the panel was on the first
    /// read after the Enter; `key if=<header row> esc` closed it eight times
    /// of eight, the header gone within 35 ms, the composer back and the
    /// transcript not one row longer. READ in the 2.1.287 bundle, never seen:
    /// the loading and failed states, an enrich error, what Escape does with
    /// the panel over a running turn (never pressed there, below). NOT
    /// EXERCISED AT ALL: a second account, and this step against a real
    /// session — the measurement was taken by hand, with the harness off.
    ///
    /// THE GATES ([`Self::probe_gates`]), any one shut and nothing is asked
    /// of the host or written. Then the host's claim
    /// ([`IdleHost::account_probe`]), and from there the host is told what
    /// came of it exactly once ([`IdleHost::account_probed`]). A DUE PROBE
    /// THAT IS NOT RUN SAYS WHICH GATE SHUT, once a point
    /// ([`Self::probe_shut`]); where the roster did not say one is due —
    /// every point of every session with no roster — nothing is asked and
    /// nothing said.
    ///
    /// EVERY WRITE IS FENCED THREE WAYS, and there is no unfenced fallback
    /// ([`Self::probe_type`]): the person count (`if-human=`), the screen
    /// generation (`if-gen=`) and the composer's caret row (`if=`) on the
    /// text and on its Enter. The Escape is fenced by the person count and
    /// by THE PANEL'S OWN HEADER ROW, checked by the server at the press
    /// ([`Self::panel_escape`]) — it cannot be written unless the panel is on
    /// the grid at that instant — and never while a busy signal shows:
    /// Escape stops a running turn.
    ///
    /// THE READ ([`usage_panel_read`]) is taken every [`ProbeWaits::step`] at
    /// the latest ([`Self::probe_pace`]), NOT once the screen has held still
    /// half a second (the spec's `await idle 500`, capped at 1.5 s): the
    /// measurement contradicts the spec's "unsettled, then settled or
    /// folded". On an ordinary grid the
    /// tab reads unsettled for a tenth to a quarter of a second (the home's
    /// last-known figures under a refresh line), SETTLED for about a second
    /// while its "contributing" block is still scanning, and FOLDED for good
    /// once that block fills in and pushes the tail off the grid. So:
    ///
    /// * two reads in a row `Settled` with EQUAL windows are the sample
    ///   (measured: the windows of the short panel equal the final ones in
    ///   every run);
    /// * two in a row folded are [`ProbeMiss::Fold`] — and the grid's size
    ///   is latched off for this loop ([`Session::probe_fold`]) ONLY where
    ///   EVERY read of this probe was the fold: a size whose tail never
    ///   shows (110x32, measured: folded from the first read, the refresh
    ///   line never on the grid) is not typed into again, while a race lost
    ///   on a loaded machine (129x47, the owner's own size: unsettled, then
    ///   settled for 1.1 s) is tried again at the roster's next due read.
    ///   The spec latched both; the measurement says the second is not a
    ///   property of the size;
    /// * two in a row that are not the Usage tab under the fullscreen
    ///   renderer's rule are [`ProbeMiss::Renderer`], and this loop probes
    ///   no more;
    /// * a person's key since the Enter is [`ProbeMiss::Person`]: nothing
    ///   is sampled from a panel a person may be driving, and no Escape goes
    ///   until their grace has passed ([`Self::usage_panel_look`]);
    /// * the panel gone by itself is [`ProbeMiss::Closed`]; the bound run
    ///   out, [`ProbeMiss::Unsettled`].
    ///
    /// THE PANEL IS OWED ITS CLOSE FROM THE WRITE ON ([`UsageProbe`]), in
    /// memory and in the session ledger, whatever happens to this step — a
    /// reply that never comes, the loop's stop. A stopped loop writes
    /// nothing ([`Session::call`] refuses it), so a stop mid-probe presses
    /// no Escape: the loop that follows reads the ledger and closes it (the
    /// spec had one Escape go on a stop; the loop's own rule is older and
    /// stricter).
    pub(super) fn usage_probe(
        &mut self,
        point: &Turn,
        decided: &TurnEndAction,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Probed, Fail> {
        let Some(host) = opts.idle_host.clone() else {
            return Ok(Probed::No);
        };
        // The roster's word first: with none, no gate is asked and nothing
        // is said.
        if !self.probe_advised(&point.screen) {
            return Ok(Probed::No);
        }
        let shut = match self.probe_gates(point, decided, host.as_ref(), opts)? {
            Some(gate) => Some((gate, None)),
            None => host
                .account_probe()
                .err()
                .map(|why| (ProbeGate::Claim, Some(why))),
        };
        if let Some((gate, why)) = shut {
            self.probe_shut(point, gate, why, review)?;
            return Ok(Probed::No);
        }
        // Claimed: the point has its probe, and no line about one not run.
        self.probe_said = Some(self.review_key(point));
        let mut tell = Telling {
            host: host.as_ref(),
            told: false,
        };
        let done = self.probe_step(point, &mut tell, opts, review);
        // A step that ended on an error with nothing told: its claim is let
        // go, and what it did is not known.
        if !tell.told {
            tell.tell(Err(ProbeMiss::Lost));
            review.note(&format!(
                "PROBED seq={} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} no sample ({})",
                point.screen.seq,
                ProbeMiss::Lost.word()
            ));
        }
        done
    }

    /// Whether the advice this point's decision was given
    /// ([`Self::account_advise`], kept for the screen it was asked on) is the
    /// probe: the roster says the account's `/usage` read is due here. The
    /// spec's fourth gate, asked FIRST: with no roster — no host, a host that
    /// keeps none, `accounts = "off"` — it is `false` before any other gate
    /// is looked at, before a request is made, and before anything is said.
    fn probe_advised(&self, screen: &Screen) -> bool {
        matches!(
            &self.account_seen,
            Some((at, AccountPoint::TurnEnd, Some(AccountAdvice::Probe { .. })))
                if *at == screen.seq
        )
    }

    /// A DUE PROBE WAS NOT RUN AT `point`: say which gate shut, ONCE A POINT
    /// — `SKIPPED seq=<n> rule=usage-probe@v1 gate=<word>` ([`ProbeGate`]),
    /// and for a claim the host refused its own word after it (`gate=claim
    /// host=<word>`). SAID, not only journaled: the line is in the session's
    /// journal and, for a hosted loop, in its host's log beside the advice
    /// that asked for the probe — where a person looking for why no `/usage`
    /// was typed reads, and a journal-only line was found by nobody.
    ///
    /// Nothing where the roster did not say a probe is due at this screen
    /// ([`Self::probe_advised`]), and nothing a second time at one point
    /// ([`Session::probe_said`]: its review key): an idle session is decided
    /// again at every wait that runs out, for hours, and its one point has
    /// one line — the first gate that shut. A point where a probe WAS
    /// claimed has none at all. A point taken after a turn ran is another
    /// point ([`Session::look`] forgets the last), though it reads the same.
    pub(super) fn probe_shut(
        &mut self,
        point: &Turn,
        gate: ProbeGate,
        host: Option<&str>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if !self.probe_advised(&point.screen) {
            return Ok(());
        }
        let key = self.review_key(point);
        if self.probe_said.as_deref() == Some(key.as_str()) {
            return Ok(());
        }
        self.probe_said = Some(key);
        let host = host.map_or_else(String::new, |word| format!(" host={word}"));
        review.say(&format!(
            "SKIPPED seq={} rule={RULE_USAGE_PROBE} gate={}{host}",
            point.screen.seq,
            gate.word()
        ))?;
        Ok(())
    }

    /// THE PROBE'S GATES, in the spec's order (§3 G) but for its fourth —
    /// the roster's advice, which [`Self::usage_probe`] asks before these
    /// ([`Self::probe_advised`]): `None` with every one open, else THE FIRST
    /// THAT SHUT ([`ProbeGate`]), for the one line the loop says of a due
    /// probe it did not run. Nothing is written by any of them, and the
    /// requests the later ones make (`help key`, `help send`, `status`,
    /// `modes`) are made only where the roster advised the probe: with no
    /// roster none of this is reached.
    fn probe_gates(
        &mut self,
        point: &Turn,
        decided: &TurnEndAction,
        host: &dyn IdleHost,
        opts: &SuperviseOpts,
    ) -> Result<Option<ProbeGate>, Fail> {
        // 1. A loop that acts on the session and keeps a ledger for it, with
        //    no panel of its own still owed and nothing that ended its
        //    probes.
        if self.claim.watching_behind().is_some() {
            return Ok(Some(ProbeGate::Watching));
        }
        if self.ledger_path.is_none() {
            return Ok(Some(ProbeGate::NoLedger));
        }
        if self.probe_off {
            return Ok(Some(ProbeGate::ProbesOff));
        }
        if self.probe != UsageProbe::Idle {
            return Ok(Some(ProbeGate::PanelOwed));
        }
        // 2. The turn that ended here was read live.
        if !self.turn_live {
            return Ok(Some(ProbeGate::NotLive));
        }
        // 3. The point is not a person's.
        if matches!(decided, TurnEndAction::Escalate { .. }) {
            return Ok(Some(ProbeGate::Escalated));
        }
        // 5. Not a grid whose tail never showed.
        let screen = &point.screen;
        if self.probe_fold == Some((screen.cols, screen.pane_rows)) {
            return Ok(Some(ProbeGate::FoldLatch));
        }
        // 6. The host asks for no point of its own, and no stall is held or
        //    suspected.
        if host.wants() {
            return Ok(Some(ProbeGate::HostWants));
        }
        if self.stalled.is_some() || self.stall_suspect {
            return Ok(Some(ProbeGate::Stalled));
        }
        // 7. Every fence the three writes carry is one this host has: a read
        //    that names its generation and its person count, `key` and
        //    `send` that take `if-gen=`, and the row guard and the wait on a
        //    row leaving not known missing.
        if screen.generation.is_none()
            || screen.human_seq.is_none()
            || self.caps.key_if == Some(false)
            || self.caps.gone == Some(false)
            || !self.gen_fence_known()?
            || !self.send_fence_known()?
        {
            return Ok(Some(ProbeGate::NoFence));
        }
        // 8. One fresh `status` — the program and a person's last key — and
        //    the reading built again on it: the policy's own gate. The
        //    program is Claude Code as the server names it NOW.
        self.program = self.foreground_program()?;
        if self.stall_suspect {
            return Ok(Some(ProbeGate::Stalled));
        }
        if self.reader(&screen.rows).program() != aterm_phase::Program::Claude {
            return Ok(Some(ProbeGate::NotClaude));
        }
        // The point was decided: its wall as the decision judged it.
        let reading = self.turn_end_reread(screen, opts);
        if let Some(gate) = self
            .turn_end
            .probe_gate(&reading, &opts.policy, Instant::now())
        {
            return Ok(Some(gate));
        }
        // 9. The fullscreen renderer.
        Ok((!self.alt_screen()?).then_some(ProbeGate::NotFullscreen))
    }

    /// Whether the session's program draws on the ALTERNATE screen (`modes`'
    /// `alt_screen=true` row — MEASURED 2026-10-01: the server prints the
    /// word, `true` or `false`, where the spec wrote `1`): Claude Code's
    /// fullscreen renderer, the one the probe types into. Its inline
    /// renderer is the main screen, and there `/usage` leaves two rows in
    /// the transcript and, once the panel has grown, scrolls its own header
    /// off the grid, where no header guard can fence an Escape (both
    /// measured). A host without the verb, or a reply with no such row, is
    /// `false`: not probed.
    fn alt_screen(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["modes"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("modes failed: {}", r.stderr.trim())));
        }
        Ok(r.ok() && r.stdout.lines().any(|l| l.trim() == "alt_screen=true"))
    }

    /// Whether the composer on `screen` holds exactly the probe's command as
    /// typed text: the loop's own words, not yet submitted.
    fn holds_probe_draft(&self, screen: &Screen) -> bool {
        let reader = self.reader(&screen.rows);
        reader
            .composer(&screen.rows)
            .is_some_and(|(_, lines)| squashed(&lines.concat()) == squashed(USAGE_COMMAND))
            && typed_draft(reader, screen)
    }

    /// A PANEL OF THE HARNESS'S MAY COME: `/usage` was written into the
    /// composer (or its write's answer never came). Owed from here, in
    /// memory and in the ledger — the `typed` row a later loop reads
    /// ([`approvals::probe_owed`]) — until a look sees the session with no
    /// panel and none of the text left ([`Self::probe_closed`]). The spec
    /// wrote the row at the Enter; it is written at the text, because text
    /// of ours that stands unsubmitted may still be sent — by a person's
    /// Enter, or by the loop where the policy sends a draft left standing
    /// ([`Self::probe_left_enter`]) — and the panel that opens then must be
    /// one the loop closes.
    fn probe_owes(&mut self, seq: u64) {
        if self.probe != UsageProbe::Idle {
            return;
        }
        self.probe = UsageProbe::Owed {
            own: true,
            since: Instant::now(),
        };
        self.ledger_row(
            RULE_USAGE_PROBE,
            approvals::Outcome::Typed,
            USAGE_COMMAND,
            approvals::PROBE_TYPED_REASON,
            seq,
        );
    }

    /// THE SESSION WAS SEEN WITH NO PANEL OF THE HARNESS'S LEFT ON IT: the
    /// ledger's `waiting` row, `CLEARED` where the journal said the panel
    /// was stuck, and the point it stood over decided again shortly
    /// ([`Self::turn_end_now`]: the act decided there may never have been
    /// carried out). Nothing where nothing was owed.
    fn probe_closed(&mut self, seq: u64, review: &mut dyn Review) {
        let was = std::mem::replace(&mut self.probe, UsageProbe::Idle);
        self.probe_hold = None;
        if was == UsageProbe::Idle {
            return;
        }
        self.ledger_row(
            RULE_USAGE_PROBE,
            approvals::Outcome::Waiting,
            USAGE_COMMAND,
            approvals::PROBE_CLOSED_REASON,
            seq,
        );
        if matches!(was, UsageProbe::Closing { .. }) {
            review.note(&format!("CLEARED seq={seq} usage panel"));
        }
        self.redecide_soon("the usage panel closed");
    }

    /// THE COMPOSER IS DRAWN on `screen` (`true`): no panel stands in its
    /// place, so a panel owed is closed ([`Self::probe_closed`]) — unless the
    /// composer still holds the probe's own command unsubmitted
    /// ([`Self::holds_probe_draft`]), which may yet be sent: owed still.
    /// `false`, and nothing done, where no composer is drawn.
    fn composer_settles(&mut self, screen: &Screen, review: &mut dyn Review) -> bool {
        if self.reader(&screen.rows).composer(&screen.rows).is_none() {
            return false;
        }
        if !self.holds_probe_draft(screen) {
            self.probe_closed(screen.seq, review);
        }
        true
    }

    /// THE PAUSE BETWEEN TWO READS OF THE PROBE'S: the screen holding still
    /// [`PROBE_STILL_MS`], or [`ProbeWaits::step`] at most, never past
    /// `until` — the server's own wait, no sleep here. A panel that sits is
    /// read twenty times a second and one that repaints without a pause four
    /// times: the cadences the live measurement read it at, either of which
    /// puts two reads inside the second the whole tab is on the grid — and
    /// never a read per repaint of a screen that animates.
    fn probe_pace(&mut self, until: Instant) -> Result<(), Fail> {
        let left = until.saturating_duration_since(Instant::now());
        self.wait(&["idle", PROBE_STILL_MS], left.min(self.probe_waits.step))?;
        Ok(())
    }

    /// The step past the claim ([`Self::usage_probe`]): type, await the
    /// panel, read it settled, tell the host, close.
    fn probe_step(
        &mut self,
        point: &Turn,
        tell: &mut Telling<'_>,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<Probed, Fail> {
        let seq = point.screen.seq;
        // T. Type.
        let (entered, human) = match self.probe_type(point, review)? {
            ProbeTyped::Entered { at, human } => (at, human),
            ProbeTyped::Not {
                miss,
                why,
                then,
                left,
            } => {
                tell.tell(Err(miss));
                review.note(&format!("SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {why}"));
                // The text stands typed and unsent: this journal line and one
                // record for a person, once — nothing is raised for it.
                if left {
                    tell.host.inform(USAGE_LEFT);
                }
                // Nothing of the point's is carried out at this look: it is
                // decided again shortly, unless a wait is set already.
                if then == Probed::Again && self.turn_end_due.is_none() {
                    self.redecide_soon("the accounts probe was not written");
                }
                return Ok(then);
            }
        };
        let no_sample = |miss: ProbeMiss, tell: &mut Telling<'_>, review: &mut dyn Review| {
            tell.tell(Err(miss));
            review.note(&format!(
                "PROBED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} no sample ({})",
                miss.word()
            ));
        };
        let waits = self.probe_waits;
        let busy = |s: &Self, grid: &Screen| s.reader(&grid.rows).phase(&grid.rows) == Phase::Busy;
        // O. Await the panel: whole reads, each after the screen's next
        // move. A turn that started instead, or no panel within the bound,
        // is the end of it — and NO ESCAPE: nothing says a panel is there to
        // take one. It stays owed, so one that comes late is still closed.
        let open_by = Instant::now() + waits.open;
        self.wait(&["seq", &entered.to_string()], waits.step.min(waits.open))?;
        let mut grid = loop {
            let grid = self.grid()?;
            if aterm_phase::settings_panel(&grid.rows).is_some() {
                break grid;
            }
            if busy(self, &grid) || Instant::now() >= open_by {
                no_sample(ProbeMiss::NotOpened, tell, review);
                return Ok(Probed::Done);
            }
            self.probe_pace(open_by)?;
        };
        // S. Read it settled.
        let settle_by = Instant::now() + waits.settle;
        let mut prior: Option<Vec<PanelWindow>> = None;
        let (mut folds, mut strange, mut tail_seen) = (0u32, 0u32, false);
        let (read, panel) = loop {
            let Some(panel) = aterm_phase::settings_panel(&grid.rows) else {
                no_sample(ProbeMiss::Closed, tell, review);
                return Ok(Probed::Done);
            };
            // A person keyed since the Enter: the panel may be theirs to
            // drive now. Nothing is sampled, and its Escape waits out their
            // grace ([`Self::usage_panel_look`]).
            if grid.human_seq != Some(human) {
                no_sample(ProbeMiss::Person, tell, review);
                return Ok(Probed::Open);
            }
            // A turn runs under the panel: no sample, and no Escape while
            // it does.
            if busy(self, &grid) {
                no_sample(ProbeMiss::Unsettled, tell, review);
                return Ok(Probed::Open);
            }
            let said = if panel.renderer == aterm_phase::PanelRenderer::Fullscreen {
                usage_panel_read(&grid.rows, panel.header)
            } else {
                PanelRead::NotUsage
            };
            folds = if said == PanelRead::Fold {
                folds + 1
            } else {
                0
            };
            strange = if said == PanelRead::NotUsage {
                strange + 1
            } else {
                0
            };
            // Any reading but the fold's saw the panel's tail region on the
            // grid: the refresh line and the cancel hint are drawn there.
            tail_seen |= matches!(said, PanelRead::Settled(_) | PanelRead::Unsettled);
            match said {
                PanelRead::Settled(windows) => {
                    if prior.as_ref() == Some(&windows) {
                        let (now, zone) = (self.now_unix(), self.zone_offset);
                        let offset = self.local_offset_s();
                        break (
                            panel_sample(&windows, now, offset, &zone).ok_or(ProbeMiss::Unsettled),
                            panel,
                        );
                    }
                    prior = Some(windows);
                }
                PanelRead::Unsettled => prior = None,
                PanelRead::Fold => {
                    prior = None;
                    if folds >= PROBE_READS {
                        // Latched only where the tail never showed: every
                        // read of this probe was the fold.
                        if !tail_seen {
                            self.probe_fold = Some((point.screen.cols, point.screen.pane_rows));
                        }
                        break (Err(ProbeMiss::Fold), panel);
                    }
                }
                PanelRead::NotUsage => {
                    prior = None;
                    if strange >= PROBE_READS {
                        self.probe_off = true;
                        break (Err(ProbeMiss::Renderer), panel);
                    }
                }
            }
            if Instant::now() >= settle_by {
                break (Err(ProbeMiss::Unsettled), panel);
            }
            self.probe_pace(settle_by)?;
            grid = self.grid()?;
        };
        // The host is told BEFORE the Escape: a close that drags on never
        // costs the sample.
        match &read {
            Ok(sample) => {
                tell.tell(Ok(sample));
                review.say(&format!(
                    "PROBED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} session={} week={}",
                    pct_word(sample.session_pct),
                    pct_word(sample.week_pct)
                ))?;
            }
            Err(miss) => no_sample(*miss, tell, review),
        }
        // C. Close.
        self.panel_escape(&grid, panel, opts, review)?;
        Ok(if self.probe == UsageProbe::Idle {
            Probed::Done
        } else {
            Probed::Open
        })
    }

    /// THE PROBE'S FENCED WRITE, a twin of [`Self::type_fenced`] with the
    /// person fence added and NO fallback: `send if-human=<n> if-gen=<the
    /// judged read> if=<its caret row> -- /usage`, the echo waited for and
    /// read as the fenced write reads it, the composer required to hold
    /// exactly the command as typed text with no box up, then `key
    /// if-human=<n'> if-gen=<that read> if=<its caret row> enter`. Nothing
    /// here ever falls back to the guarded `turn`, which pastes before its
    /// guard is checked.
    ///
    /// A write the server skipped or turned away wrote nothing
    /// ([`ProbeTyped::Not`]): for a person or a moved screen the point is
    /// decided again shortly; a guard that matched no row of the unchanged
    /// screen leaves the point's own act to go on (and to miss, and say so,
    /// the same way); a hold is parked on and a busy server backed off
    /// from.
    ///
    /// TEXT WRITTEN AND NOT SUBMITTED IS A RECORD, NEVER AN ESCALATION
    /// ([`ProbeTyped::Not`]'s `left`; alerts have a class, so this is not
    /// escalated as every other fenced write's leftover is,
    /// [`Self::left_typed`] — a ruling against the spec's row T for this exit
    /// in `docs/SPEC-claude-accounts-probe-2026-10-01.md`, which still says
    /// escalate):
    /// the journal's `SKIPPED … typed, not submitted: <why>` and one record
    /// through [`IdleHost::inform`] ([`USAGE_LEFT`]). No attention is set,
    /// no ask posted, no badge kept: nothing here is a state a person is
    /// needed for now. The panel stays owed ([`Self::probe_owes`]), the
    /// probe is not typed again over its own text (the first of
    /// [`Self::probe_gates`], and the policy's own gate on a composer that
    /// is not empty), and what becomes of the text is
    /// [`Self::probe_left_enter`]'s.
    fn probe_type(&mut self, point: &Turn, review: &mut dyn Review) -> Result<ProbeTyped, Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let not = |miss: ProbeMiss, why: &str, then: Probed| {
            Ok(ProbeTyped::Not {
                miss,
                why: why.to_string(),
                then,
                left: false,
            })
        };
        // Written and not submitted: the point is decided again shortly.
        let left = |miss: ProbeMiss, why: &str| {
            Ok(ProbeTyped::Not {
                miss,
                why: format!("typed, not submitted: {why}"),
                then: Probed::Again,
                left: true,
            })
        };
        let (Some(generation), Some(human), Some(caret)) = (
            point.screen.generation.clone(),
            point.screen.human_seq,
            caret_row(self.reader(rows), rows),
        ) else {
            return not(
                ProbeMiss::NotWritten,
                "the judged read carries no fence; nothing written",
                Probed::No,
            );
        };
        let person = format!("if-human={human}");
        let fence = format!("if-gen={generation}");
        let guard = format!("if={}", super::super::policy::row_guard(&rows[caret]));
        let r = self.call(&["send", &person, &fence, &guard, "--", USAGE_COMMAND])?;
        match self.refused_press(&r, &format!("{person} {fence} {guard} (send)"))? {
            None => {}
            Some(Pressing::Lost { why, .. }) => {
                // Cut mid-exchange, the text may be in the composer.
                if !r.untouched() {
                    self.probe_owes(seq);
                }
                return Err(Fail::Lost(why));
            }
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                return not(
                    ProbeMiss::NotWritten,
                    "the session is halted; nothing written",
                    Probed::Again,
                );
            }
            Some(Pressing::Refused { why }) => {
                self.back_off(&why, seq, None, review)?;
                return not(
                    ProbeMiss::NotWritten,
                    "the server turned the write away; nothing written",
                    Probed::Again,
                );
            }
            Some(Pressing::Unconfirmed { why, .. }) => {
                // Refused outright (a usage line): this host has no fenced
                // `send` after all, and nothing was written.
                self.caps.send_gen = Some(false);
                return not(ProbeMiss::NotWritten, &why, Probed::No);
            }
            Some(_) => {
                return not(ProbeMiss::NotWritten, "nothing written", Probed::No);
            }
        }
        if r.skipped_person() {
            return not(
                ProbeMiss::Person,
                "a person keyed before the write; nothing written",
                Probed::Again,
            );
        }
        if r.skipped_changed() {
            return not(
                ProbeMiss::NotWritten,
                "the screen moved before the write; nothing written",
                Probed::Again,
            );
        }
        if r.skipped() {
            return not(
                ProbeMiss::NotWritten,
                "the guard matched no row of the judged screen; nothing written",
                Probed::No,
            );
        }
        // Written: from here a panel of the harness's may come.
        self.probe_owes(seq);
        let at = r.seq().unwrap_or(seq);
        self.wait(&["seq", &at.to_string()], REACTION_WAIT)?;
        self.wait(&["idle", SETTLE_MS], SETTLE_CAP)?;
        let now = self.screen()?;
        let reader = self.reader(&now.rows);
        let held = self.holds_probe_draft(&now) && reader.prompt(&now.rows).is_none();
        let (true, Some(generation), Some(human), Some(caret)) = (
            held,
            now.generation.clone(),
            now.human_seq,
            caret_row(reader, &now.rows),
        ) else {
            return left(
                ProbeMiss::NotWritten,
                "the composer does not show it as written",
            );
        };
        let guard = super::super::policy::row_guard(&now.rows[caret]);
        let person = format!("if-human={human}");
        let (r, args) = self.fenced_key(&guard, "enter", &generation, Some(&person))?;
        let stamp = r.seq().unwrap_or(now.seq);
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => Ok(ProbeTyped::Entered { at: stamp, human }),
            None if r.skipped_person() => left(ProbeMiss::Person, "a person keyed before Enter"),
            None => left(ProbeMiss::NotWritten, "the screen moved before Enter"),
            Some(Pressing::Lost { why, .. }) => Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => {
                self.park_on_hold(&why, seq, review)?;
                left(ProbeMiss::NotWritten, "the session is halted")
            }
            Some(Pressing::Refused { why }) | Some(Pressing::Unconfirmed { why, .. }) => {
                left(ProbeMiss::NotWritten, &one_line(&why))
            }
            Some(_) => left(ProbeMiss::NotWritten, "its Enter was not written"),
        }
    }

    /// WHETHER THE PROBE'S OWN `/usage` STANDS TYPED AND UNSENT on `screen`:
    /// a panel is owed — this loop wrote the command, or the ledger says a
    /// loop before it did and none has seen its panel gone — and the
    /// composer holds exactly the command as typed text
    /// ([`Self::holds_probe_draft`]). With nothing owed a `/usage` in the
    /// composer is a person's draft, as any is.
    fn probe_left_stands(&self, screen: &Screen) -> bool {
        self.probe != UsageProbe::Idle && self.holds_probe_draft(screen)
    }

    /// THE PROBE'S OWN `/usage`, LEFT TYPED ([`Self::probe_type`]), SENT —
    /// where the turn-end policy, reading it as the draft it is, sends the
    /// draft in an act's place ([`TurnEndAction::Submit`]: past a person's
    /// grace since the text last changed, at a point that has an act).
    /// [`Self::turn_end_execute`] hands that Enter here, and it is the
    /// PROBE'S, not the act's: fenced as the probe's Enter is (`key
    /// [if-human=<n>] if-gen=<the judged read> if=<its caret row> enter`),
    /// journaled under the probe's rule, and BOOKED NOWHERE in the policy —
    /// no [`TurnEndState::acted`], no `CONTINUED`, no `typed` row under the
    /// act's rule. Booked as the act, it would spend one of the hour's
    /// continuations, and the worker — which does no work for a `/usage` —
    /// would read as a short turn not taken, so its real continuation would
    /// wait out [`TurnEndTiming::take_within`] and a step of the back-off.
    ///
    /// The panel the Enter opens is the harness's and owed
    /// ([`Self::probe_owes`], from the text): the next look closes it with
    /// its one guarded Escape and samples nothing
    /// ([`Self::usage_panel_look`]), and the point is then decided again
    /// ([`Self::probe_closed`]) — on an empty composer, where its own act is
    /// typed. An Enter the server wrote that did NOT take (the text still
    /// stands, the screen as it was) is found at the re-decision set here,
    /// [`TurnEndTiming::take_within`] on — the wait a typed act is given —
    /// and written again there: never a text left standing with no look to
    /// come.
    ///
    /// An Enter the server skipped (a person keyed, the screen moved) leaves
    /// the text and decides the point again shortly; a hold is parked on, a
    /// busy server backed off from; one refused outright, or a read with no
    /// generation to fence on, presses nothing. None of them raises
    /// anything: the text stands, a person's Enter sends it as well, and the
    /// panel is closed whoever sent it.
    fn probe_left_enter(&mut self, point: &Turn, review: &mut dyn Review) -> Result<(), Fail> {
        let seq = point.screen.seq;
        let rows = &point.screen.rows;
        let skipped = |why: &str, review: &mut dyn Review| {
            review.note(&format!(
                "SKIPPED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} stays typed: {why}"
            ));
        };
        let (Some(generation), Some(caret)) = (
            point.screen.generation.clone(),
            caret_row(self.reader(rows), rows),
        ) else {
            skipped("the judged read carries no fence for its Enter", review);
            return Ok(());
        };
        let guard = super::super::policy::row_guard(&rows[caret]);
        let person = point.screen.human_seq.map(|n| format!("if-human={n}"));
        let (r, args) = self.fenced_key(&guard, "enter", &generation, person.as_deref())?;
        match self.refused_press(&r, &args)? {
            None if !r.skipped() => {
                review.note(&format!(
                    "TYPED seq={seq} rule={RULE_USAGE_PROBE} {USAGE_COMMAND} left typed, its \
                     Enter written now: no act of the policy's, and the panel it opens is closed \
                     unsampled"
                ));
                // An Enter that did not take leaves the text standing, and
                // no look says so: the point is decided again after the wait
                // a typed act is given to be taken. One that took moves the
                // screen first, and the close sets its own.
                let due = Instant::now() + self.turn_end.timing.take_within;
                self.set_due(due, "the accounts probe's text was sent");
            }
            None if r.skipped_person() || r.skipped_changed() => {
                skipped("a person keyed or the screen moved before Enter", review);
                self.redecide_soon("the accounts probe's text was not sent");
            }
            None => skipped("its caret row matched no row of the judged screen", review),
            Some(Pressing::Lost { why, .. }) => return Err(Fail::Lost(why)),
            Some(Pressing::Halted { why }) => self.park_on_hold(&why, seq, review)?,
            Some(Pressing::Refused { why }) => self.back_off(&why, seq, None, review)?,
            Some(Pressing::Unconfirmed { why, .. }) => skipped(&one_line(&why), review),
            Some(_) => {}
        }
        Ok(())
    }

    /// ONE GUARDED ESCAPE AT THE PANEL ON `grid`, and what came of it: `key
    /// if-human=<the read's person count> if=<the panel's header row> esc`
    /// ([`Self::guarded_key`]; no `if-gen=`: a panel that refreshes moves
    /// the generation), then the header row's leaving waited for
    /// ([`ProbeWaits::close`]) and the grid read again.
    ///
    /// * The panel gone and the composer back: closed
    ///   ([`Self::probe_closed`]). Gone with no composer drawn yet: left
    ///   owed, for the next look to settle.
    /// * Still open: a failed try — the next one a pause of the survey's
    ///   ladder away ([`SURVEY_RETRY`]: 5 s first, far outside the vendor's
    ///   800 ms double-Escape window, the last pause for ever), `WAITING`
    ///   journaled, and at the [`PROBE_STUCK_AT`]-th the host told once
    ///   ([`IdleHost::inform`]) while the tries go on.
    /// * Skipped because no row matched: judged by the same read — the
    ///   panel left first (closed), or it stands under a row the guard does
    ///   not match, which is a try that failed. Skipped for a person's key:
    ///   no try spent. Turned away (`ERR halted`, `ERR busy`, `ERR rate`):
    ///   parked or backed off, no try spent. The next look reads again.
    /// * A read with no person count, or a host with no `key if=`: NOTHING
    ///   is pressed — never an unfenced Escape — and it counts as a try
    ///   that failed, so a person is told.
    ///
    /// The caller has checked what the server cannot: that no busy signal
    /// shows, that no person is within their grace, that a panel taken up
    /// from the ledger has stood one grace.
    pub(super) fn panel_escape(
        &mut self,
        grid: &Screen,
        panel: aterm_phase::SettingsPanel,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let seq = grid.seq;
        let guard = panel.header_guard(&grid.rows);
        if !guard.is_empty() && grid.human_seq.is_some() {
            let judged = match self.guarded_key(seq, &guard, "esc", grid.human_seq)? {
                Dismiss::Pressed { .. } => {
                    self.wait_row_gone(&guard, seq, self.probe_waits.close)?;
                    true
                }
                // No row matched at the press: the panel left first — or it
                // stands and its row is not the one read, which a read says.
                Dismiss::Skipped => true,
                Dismiss::Yielded => return Ok(()),
                Dismiss::Refused { halted, why } => {
                    return self.refused(halted, &why, seq, review);
                }
                Dismiss::Unguarded => false,
            };
            if judged {
                let after = self.grid()?;
                if aterm_phase::settings_panel(&after.rows).is_none() {
                    self.composer_settles(&after, review);
                    return Ok(());
                }
            }
        }
        let (own, since, failed, told) = match self.probe {
            UsageProbe::Idle => return Ok(()),
            UsageProbe::Owed { own, since } => (own, since, 0, false),
            UsageProbe::Closing {
                own,
                since,
                tries,
                told,
                ..
            } => (own, since, tries, told),
        };
        let tries = failed.saturating_add(1);
        let pause = crate::supervise::ladder::Ladder(&self.survey_retry)
            .step(usize::try_from(tries - 1).unwrap_or(usize::MAX));
        let stuck = tries >= PROBE_STUCK_AT && !told;
        self.probe = UsageProbe::Closing {
            own,
            since,
            tries,
            at: Instant::now() + pause,
            told: told || stuck,
        };
        review.note(&format!(
            "WAITING seq={seq} the {USAGE_COMMAND} panel is open ({tries} tries): Escape again in \
             {} s",
            pause.as_secs()
        ));
        if stuck && let Some(host) = opts.idle_host.as_ref() {
            host.inform(PANEL_STUCK);
        }
        Ok(())
    }

    /// THE PANEL OWED ITS CLOSE, at the top of every look
    /// ([`Session::look`], before anything of the look is decided; the
    /// spec's §3.1). Nothing while none is owed. `true`: a panel was handled
    /// — waited on, or its Escape pressed — and the loop looks again, so no
    /// decision is ever made on a panel screen.
    ///
    /// * The turn shows the composer: no panel stands in its place. Closed
    ///   ([`Self::composer_settles`]) — unless the composer still holds the
    ///   probe's own command unsubmitted, which may yet be sent: owed still,
    ///   and the look goes on as any other.
    /// * Else one whole read. No Settings panel on it: closed, and the loop
    ///   reads again (the turn was handed over without waiting for the
    ///   screen to hold still, [`Session::await_turn_from`]) unless it is a
    ///   box, which is judged as read.
    /// * A panel. In order, each a wait that ends the moment the header
    ///   row leaves: a stall of the worker's is held, never pressed into; a
    ///   busy signal — no Escape while one shows, and the vendor retires
    ///   the panel at the turn's end (READ, not measured); a loop that only
    ///   watches behind another supervisor's claim — the panel is the
    ///   holder's to close, and an Escape of this loop's would be refused
    ///   here ([`Session::claim_refuses`]); a panel taken up
    ///   from the ledger stands one person's grace in this loop's sight
    ///   first; a person within their grace on a fresh `status` (`HELD`,
    ///   said once a hold); a failed Escape's pause not yet over. Then the
    ///   guarded Escape ([`Self::panel_escape`]).
    ///
    /// What is NOT closed: a Settings panel on a session whose loop wrote no
    /// `/usage` and whose ledger holds no unanswered row of one — a
    /// person's own. The residual the spec states: a person who opens
    /// `/usage` right after a loop died mid-probe has it closed after a
    /// grace with nobody typing.
    pub(super) fn usage_panel_look(
        &mut self,
        turn: &Turn,
        state: &mut Looking,
        deadline: Instant,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let (own, since) = match self.probe {
            UsageProbe::Idle => return Ok(false),
            UsageProbe::Owed { own, since } | UsageProbe::Closing { own, since, .. } => {
                (own, since)
            }
        };
        let seq = turn.screen.seq;
        if self.composer_settles(&turn.screen, review) {
            return Ok(false);
        }
        let grid = self.grid()?;
        let Some(panel) = aterm_phase::settings_panel(&grid.rows) else {
            self.probe_closed(seq, review);
            return Ok(turn.phase != Phase::Prompt);
        };
        let guard = panel.header_guard(&grid.rows);
        // One wait on the header row leaving, to `until` at most: a step of
        // the loop's and the budget bound it.
        let step = |until: Option<Instant>| {
            let now = Instant::now();
            until
                .map_or(WAIT_STEP, |t| t.saturating_duration_since(now))
                .min(WAIT_STEP)
                .min(deadline.saturating_duration_since(now))
        };
        if self.stall_look(seq, state, review)?
            || self.reader(&grid.rows).phase(&grid.rows) == Phase::Busy
            || self.claim.watching_behind().is_some()
        {
            self.wait_row_gone(&guard, grid.seq, step(None))?;
            return Ok(true);
        }
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        if !own && since.elapsed() < grace {
            self.wait_row_gone(&guard, grid.seq, step(since.checked_add(grace)))?;
            return Ok(true);
        }
        if let Some(until) = self.probe_hold.filter(|until| *until > Instant::now()) {
            self.wait_row_gone(&guard, grid.seq, step(Some(until)))?;
            return Ok(true);
        }
        self.program = self.foreground_program()?;
        self.probe_hold = self.hands_off_until(opts);
        if let Some(until) = self.probe_hold {
            review.note(&self.held_line(seq, until, "the usage panel's Escape"));
            self.wait_row_gone(&guard, grid.seq, step(Some(until)))?;
            return Ok(true);
        }
        if let UsageProbe::Closing { at, .. } = self.probe
            && Instant::now() < at
        {
            self.wait_row_gone(&guard, grid.seq, step(Some(at)))?;
            return Ok(true);
        }
        self.panel_escape(&grid, panel, opts, review)?;
        Ok(true)
    }

    /// D. THE POINT DECIDED AGAIN AFTER A PROBE ([`Probed::Done`]; the
    /// spec's P21): one ordinary read — which settles a panel still owed
    /// when it shows the composer — and, where it shows the SAME point (its
    /// review key), [`Self::turn_end_now`]: the host is asked afresh, so a
    /// sample that shows the account low moves the session here, and
    /// otherwise the point's own act is decided and carried out as it would
    /// have been. `turn_end_now` never probes: the turn it decides on was
    /// not read live ([`Session::turn_live`] is this look's, and the step is
    /// not called from there).
    ///
    /// A screen with NO COMPOSER while the panel is still owed is no point
    /// at all: a close not yet painted (a panel closed under the read,
    /// caught half erased), or a panel that came late. Nothing is carried
    /// out on it, and the point is decided again shortly on a fresh read;
    /// the panel stays owed until a look sees it gone, and one that stands
    /// gets its guarded Escape there ([`Self::usage_panel_look`]).
    ///
    /// A screen WITH its composer that shows ANOTHER point is the probe
    /// having left something on it — rows in the transcript, a turn
    /// started: nothing is carried out, this loop probes no more, and the
    /// next look takes the screen as the new point it is.
    pub(super) fn after_probe(
        &mut self,
        point: &Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let screen = self.screen()?;
        let turn = self.turn_of(screen);
        if !self.composer_settles(&turn.screen, review) && self.probe != UsageProbe::Idle {
            self.redecide_soon("the usage panel is closing");
            return Ok(());
        }
        if self.review_key(&turn) != self.review_key(point) {
            self.probe_off = true;
            review.note(&format!(
                "SKIPPED seq={} rule={RULE_USAGE_PROBE} the point changed across the probe: \
                 nothing is carried out on it, and this loop probes no more",
                turn.screen.seq
            ));
            return Ok(());
        }
        self.turn_end_now(point, opts, allow, review)
    }
}

/// Whether the table's `action` at a point that asks a question in prose is
/// one the session's priority word can change ([`Session::decide_to_act`]):
/// an answer (the word picks its priority), or the escalation
/// `answer_questions = false` makes ([`ANSWERS_OFF`]). A wait, a cap's or a
/// wall's escalation is the table's alone, and reads no word.
fn session_word_can_decide(action: &TurnEndAction) -> bool {
    match action {
        TurnEndAction::Escalate { reason } => reason.ends_with(ANSWERS_OFF),
        action => action
            .rule_id()
            .is_some_and(super::super::policy::turn_end::is_answer_rule),
    }
}
