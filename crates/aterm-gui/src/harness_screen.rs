// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings ▸ Harness read projection (`docs/DESIGN-harness-autopilot-
//! 2026-10-15.md` §4) — the harness analogue of [`crate::packages_screen`].
//! ONE plain-data snapshot, [`HarnessState`], drives the native `/harness`
//! route: the App builds it ([`App::harness_state`]) from what it already
//! holds — the policy in force, every live supervisable agent session's
//! Autopilot, the ledgers' last reading and the A/B clock — and publishes it
//! to every Settings view by revision fan-out (`App::publish_native_harness_
//! state`, `AppEvent::HarnessChanged`). The page renders its
//! [`HarnessProjection`], a pure function of the state, so what the screen
//! shows and what `controls prefs` reads are one derivation.
//!
//! **Nothing polls.** The projection is published when the page opens, on a
//! Refresh press, when a session's Autopilot is written, when the policy in
//! force changes, when the ledgers' reading lands, and — while a view shows
//! the page — on the host's notes and a session's claim moving.
//!
//! **What the harness did** (§4 item 4) is folded from the ledgers it already
//! writes by [`aterm_agent::harness::stats::stats_under`], which is blocking
//! file I/O: it runs on a one-shot background thread
//! ([`App::start_harness_stats_refresh`]) for two windows — today (since the
//! local midnight) and the last 7 days — and comes back as
//! `Wake::HarnessStatsCollected`. It creates nothing.
//!
//! **With and without** (§4 item 5, the A/B) is measured here, in-process:
//! per session, how long its agent spent BUSY and how long it spent WAITING
//! at a point for input (the published agent words `idle`, `prompt` and
//! `question`), split by the Autopilot state it ran under ([`AbArm`]):
//! Cruise and Land under this host's live claim, Off — plain Claude Code, no
//! harness's hands on it: its word Off, the master switch off, or no host,
//! and nobody's claim live — and the rest, unsupervised. The page compares
//! Cruise with Off only: a landing ENDS waiting ("landed, then nothing more
//! typed"), so Land's share is shown on its own and never set beside
//! Cruise's. The clock ([`AbBook`]) charges the time since the last change to
//! the bucket in force at each change — a word that moved, or an arm that
//! moved — so it reads no clock on the render path, and lives for this
//! process only. No "time saved" figure is derived from it: nothing measures
//! one (§6). An agent that has finished its work waits too, charged like any
//! other wait, and the card says so ([`ab_rows`]).

use std::collections::HashMap;
use std::path::Path;

use aterm_agent::harness::stats::HarnessStats;
use aterm_agent::supervise::{Autopilot, SupervisorConfig};

use crate::App;
use crate::autopilot::{AutopilotView, HarnessPosture};
use crate::claude_lights::{ChipFace, ChipGlass};

/// The 7-day window's length.
const WEEK_MS: i64 = 7 * 86_400_000;

// ---------------------------------------------------------------------------
// The A/B clock
// ---------------------------------------------------------------------------

/// Which arm of the A/B a stretch of an agent's time falls in (§4 item 5):
/// the Autopilot state it ran under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbArm {
    /// Cruise under this host's live claim: the harness kept the work moving.
    Cruise,
    /// Land under this host's live claim: the harness was wrapping the work
    /// up. A landing ends waiting, so this share is its own, never compared.
    Land,
    /// Plain Claude Code, the baseline: no harness's hands on the session —
    /// its Autopilot Off, the master switch off, or no host here — and no
    /// supervisor's claim live on it.
    Off,
    /// None of these: a mode the harness carries with no claim of this
    /// host's (the agent not yet attached, another supervisor's claim), or
    /// Off while a claim is still live. Counted, so no time is dropped; never
    /// compared.
    Unsupervised,
}

impl AbArm {
    /// The arm of a session whose effective Autopilot is `view`, whose live
    /// claim is this host's (`supervised`), and on which anyone's claim is
    /// live (`claimed`; `supervised` implies it).
    #[must_use]
    pub(crate) const fn of(view: AutopilotView, supervised: bool, claimed: bool) -> Self {
        match view {
            AutopilotView::Cruise if supervised => Self::Cruise,
            AutopilotView::Land if supervised => Self::Land,
            AutopilotView::Off | AutopilotView::MasterOff | AutopilotView::Unhosted if !claimed => {
                Self::Off
            }
            _ => Self::Unsupervised,
        }
    }
}

/// What an agent was doing in a stretch the A/B counts: busy, or stopped at a
/// point waiting for input. Every other published word — a wall, a survey,
/// `unknown`, `-` — is counted in neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbPhase {
    /// `busy`: a turn (or background work) runs.
    Busy,
    /// `idle`, `prompt` or `question`: the agent waits for an answer, an
    /// approval or its next instruction.
    Waiting,
}

impl AbPhase {
    /// The phase a published agent word (`AgentPublication::word`) counts as.
    #[must_use]
    pub(crate) fn of(word: &str) -> Option<Self> {
        match word {
            "busy" => Some(Self::Busy),
            "idle" | "prompt" | "question" => Some(Self::Waiting),
            _ => None,
        }
    }
}

/// One arm's totals: how long agents were busy and how long they waited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbArmTotals {
    /// Milliseconds busy.
    pub(crate) busy_ms: u64,
    /// Milliseconds stopped at a point waiting for input.
    pub(crate) waiting_ms: u64,
}

impl AbArmTotals {
    /// All the time this arm measured.
    #[must_use]
    pub(crate) const fn measured_ms(self) -> u64 {
        self.busy_ms.saturating_add(self.waiting_ms)
    }

    fn add(&mut self, phase: AbPhase, ms: u64) {
        let bucket = match phase {
            AbPhase::Busy => &mut self.busy_ms,
            AbPhase::Waiting => &mut self.waiting_ms,
        };
        *bucket = bucket.saturating_add(ms);
    }
}

/// The A/B totals for this aterm run ([`App::harness_ab`]): each arm's busy
/// and waiting time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbTotals {
    /// Under Cruise ([`AbArm::Cruise`]).
    pub(crate) cruise: AbArmTotals,
    /// Under Land ([`AbArm::Land`]).
    pub(crate) land: AbArmTotals,
    /// With the harness off ([`AbArm::Off`]).
    pub(crate) off: AbArmTotals,
    /// Unsupervised ([`AbArm::Unsupervised`]).
    pub(crate) unsupervised: AbArmTotals,
}

impl AbTotals {
    fn arm_mut(&mut self, arm: AbArm) -> &mut AbArmTotals {
        match arm {
            AbArm::Cruise => &mut self.cruise,
            AbArm::Land => &mut self.land,
            AbArm::Off => &mut self.off,
            AbArm::Unsupervised => &mut self.unsupervised,
        }
    }
}

/// One session's open stretch: the phase and arm in force since `since_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AbClock {
    phase: AbPhase,
    arm: AbArm,
    since_ms: u64,
}

/// THE A/B CLOCK: each session's open stretch and the settled totals. Fed at
/// each change ([`Self::observe`]) and at a session's end ([`Self::retire`]);
/// read with the open stretches counted up to a given instant
/// ([`Self::totals_at`]). Times are on the ledger clock
/// (`turn_ledger::now_ms`), the one a published word's `changed_ms` is on.
#[derive(Debug, Default)]
pub(crate) struct AbBook {
    /// Session → its open stretch. A session counted in no phase has none.
    clocks: HashMap<u64, AbClock>,
    /// Every closed stretch, summed.
    settled: AbTotals,
}

impl AbBook {
    /// Session `session` is in `phase` under `arm` from `at_ms` on: the time
    /// since its last change goes to the bucket that was in force (the arm
    /// and phase of then, never of now), and a new stretch opens. An instant
    /// before the open stretch's start counts nothing (never a negative).
    pub(crate) fn observe(&mut self, session: u64, phase: Option<AbPhase>, arm: AbArm, at_ms: u64) {
        let at_ms = match self.clocks.remove(&session) {
            Some(open) => {
                let at = at_ms.max(open.since_ms);
                self.settled
                    .arm_mut(open.arm)
                    .add(open.phase, at - open.since_ms);
                at
            }
            None => at_ms,
        };
        if let Some(phase) = phase {
            self.clocks.insert(
                session,
                AbClock {
                    phase,
                    arm,
                    since_ms: at_ms,
                },
            );
        }
    }

    /// Session `session` ended at `at_ms`: its open stretch is settled and
    /// the clock forgets it.
    pub(crate) fn retire(&mut self, session: u64, at_ms: u64) {
        if let Some(open) = self.clocks.get(&session).copied() {
            self.observe(session, None, open.arm, at_ms);
        }
    }

    /// The totals at `now_ms`: the settled stretches and every open one up
    /// to `now_ms`.
    #[must_use]
    pub(crate) fn totals_at(&self, now_ms: u64) -> AbTotals {
        let mut totals = self.settled;
        for open in self.clocks.values() {
            totals
                .arm_mut(open.arm)
                .add(open.phase, now_ms.saturating_sub(open.since_ms));
        }
        totals
    }
}

// ---------------------------------------------------------------------------
// What the harness did
// ---------------------------------------------------------------------------

/// One reading of the ledgers: today and the last 7 days, and when it was
/// taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessStatsReport {
    /// Since the local midnight (UTC's, where no offset could be read).
    pub(crate) today: HarnessStats,
    /// Since 7 days before the reading.
    pub(crate) week: HarnessStats,
    /// When the reading was taken, unix ms.
    pub(crate) collected_unix_ms: i64,
    /// The local zone's offset east of UTC at that instant, in seconds, or
    /// `None` where it could not be read (today is then UTC's day).
    pub(crate) offset_s: Option<i64>,
}

/// The unix ms of the local midnight that starts the day `now_ms` falls in.
/// `offset_at` is the zone's offset east of UTC in force at a unix second
/// (`None`: unknown, and the day is UTC's). The DATE is now's, under now's
/// offset; the midnight is that date's 00:00 under the offset in force AT
/// midnight — on the day daylight saving starts or ends the two differ by
/// the shift, and now's offset would start "today" that much off.
///
/// Midnight's offset is read at the instant now's offset puts midnight at,
/// and an hour before it: where clocks fall back AT 01:00 local (to 00:00:
/// Atlantic/Azores, America/Havana) the date's 00:00 comes twice, and the
/// instant now's offset gives is the second, so the day would start an hour
/// late. Each offset read is a candidate, kept only where the zone really
/// is at it then (its instant reads 00:00 on that date); the earliest kept
/// wins. None kept is a midnight the clocks skip (a spring forward AT
/// 00:00): the day starts where the skip lands, the later read's instant.
/// A shift of up to an hour is covered (a longer one at midnight would
/// start the day late by the excess). Pure.
#[must_use]
pub(crate) fn local_midnight_ms(now_ms: i64, offset_at: impl Fn(i64) -> Option<i64>) -> i64 {
    const DAY_MS: i64 = 86_400_000;
    const HOUR_S: i64 = 3_600;
    let ms = |offset_s: i64| offset_s.saturating_mul(1000);
    let Some(now_offset) = offset_at(now_ms.div_euclid(1000)) else {
        return now_ms - now_ms.rem_euclid(DAY_MS);
    };
    // The offset at a unix second, now's where it cannot be read.
    let at = |unix_s: i64| offset_at(unix_s).unwrap_or(now_offset);
    let local = now_ms.saturating_add(ms(now_offset));
    // The date's 00:00 on the local clock, as if the zone were UTC.
    let day = local - local.rem_euclid(DAY_MS);
    let guess_s = day.saturating_sub(ms(now_offset)).div_euclid(1000);
    let late = at(guess_s);
    let early = at(guess_s.saturating_sub(HOUR_S));
    [early, late]
        .into_iter()
        .filter_map(|offset| {
            let instant = day.saturating_sub(ms(offset));
            (at(instant.div_euclid(1000)) == offset).then_some(instant)
        })
        .min()
        .unwrap_or_else(|| day.saturating_sub(ms(late)))
}

/// READ THE LEDGERS (blocking file I/O: a worker's only): what the harness
/// did since the local midnight and over the last 7 days, under aterm's
/// state directory (`state_dir`, its `drive/` ledgers and journals) and the
/// harness's own (`harness_state`, the live upgrade's ledger). `offset_at` is
/// the zone's offset at a unix second ([`local_midnight_ms`]'s). Creates
/// nothing; a missing file or directory is no rows. `Err` where either
/// directory cannot be resolved here — there is nothing to read.
pub(crate) fn collect_harness_stats(
    state_dir: Option<&Path>,
    harness_state: Option<&Path>,
    now_ms: i64,
    offset_at: impl Fn(i64) -> Option<i64>,
) -> Result<HarnessStatsReport, String> {
    let (Some(state_dir), Some(harness_state)) = (state_dir, harness_state) else {
        return Err("no state directory resolves here".to_string());
    };
    let read = |since| aterm_agent::harness::stats::stats_under(state_dir, harness_state, since);
    let offset_s = offset_at(now_ms.div_euclid(1000));
    Ok(HarnessStatsReport {
        today: read(local_midnight_ms(now_ms, &offset_at)),
        week: read(now_ms.saturating_sub(WEEK_MS)),
        collected_unix_ms: now_ms,
        offset_s,
    })
}

/// The ledgers' reading as the page has it: the last report, whether a
/// reading is running, and why none could be taken.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HarnessStatsSlot {
    /// The newest reading, boxed (two folds of the ledgers).
    pub(crate) report: Option<Box<HarnessStatsReport>>,
    /// A reading is running ([`App::start_harness_stats_refresh`]).
    pub(crate) collecting: bool,
    /// Why the last reading has nothing (no state directory, no thread).
    pub(crate) unavailable: Option<String>,
}

// ---------------------------------------------------------------------------
// The App's own part
// ---------------------------------------------------------------------------

/// What the App keeps for Settings ▸ Harness: the projection's revision, the
/// A/B clock and the ledgers' reading.
#[derive(Debug, Default)]
pub(crate) struct HarnessScreen {
    /// The last published revision (bumped by every publish).
    pub(crate) revision: u64,
    /// The A/B clock ([`AbBook`]).
    pub(crate) ab: AbBook,
    /// The ledgers' reading.
    pub(crate) stats: HarnessStatsSlot,
    /// The running reading's ticket: a completion for an older one is dropped.
    sequence: u64,
}

impl HarnessScreen {
    /// Session `session`'s published agent word just moved (a sweep's
    /// publish, an input stall's): the stretch it closes goes to the bucket in
    /// force, and the new word's opens at the instant the word moved (its
    /// `changed_ms`, so no clock is read here). `arm` is the session's arm
    /// now. A session whose program is no supervisable agent counts in no
    /// phase — a shell's idle hours are no agent's wait.
    pub(crate) fn note_phase(&mut self, session: u64, ctx: &crate::SessionCtx, arm: AbArm) {
        // The timeline's guard ends before the word is read as a phase: the
        // session's locks are taken meta, then timeline, never the reverse
        // (`aterm-census --locks`).
        let (agent, word, at_ms) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let agent = tl.agent();
            (
                crate::harness_host::agent_of(agent),
                agent.word,
                agent.changed_ms,
            )
        };
        let phase = agent.and_then(|_| AbPhase::of(word));
        self.ab.observe(session, phase, arm, at_ms);
    }
}

// ---------------------------------------------------------------------------
// The published state
// ---------------------------------------------------------------------------

/// One live supervisable agent session as the page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessSession {
    /// Its process-local id: what a press names.
    pub(crate) local_id: u64,
    /// Its tab title (the person's title, else the live one).
    pub(crate) title: String,
    /// Where its tab is (`in tab 2`, `in window 1, tab 2`, `in its tab`).
    pub(crate) place: String,
    /// The agent's product name ([`aterm_phase::Program::product`]): `Claude
    /// Code`, `Codex`, `Antigravity CLI`.
    pub(crate) program: &'static str,
    /// Its own Autopilot word (`None`: the table's default decides).
    pub(crate) word: Option<Autopilot>,
    /// Its effective Autopilot under its own posture (the window's, joined
    /// with its agent's own switch: [`HarnessPosture::for_agent`]).
    pub(crate) view: AutopilotView,
    /// The default a press of its Cruise, Land or Off is weighed against
    /// ([`hands_back`]) while the harness can steer it — its own posture's
    /// ([`HarnessPosture::default_mode`]); `None` where it can steer none
    /// (no host, the master switch or its agent's own switch Off), and its
    /// buttons are inert.
    pub(crate) default: Option<Autopilot>,
    /// Whether this host's live claim is on it.
    pub(crate) supervised: bool,
    /// The harness's glass on its Autopilot chip (`App::chip_glass`): the
    /// act it just took, its task's face, its timer — what the row's state
    /// line says (§9.3.4).
    pub(crate) glass: ChipGlass,
}

/// One `[harness]` key of the effective table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessConfigRow {
    /// The key (`approve`; an agent's table's `codex.enabled`).
    pub(crate) key: String,
    /// Its value, as the reader takes it back (`safe`).
    pub(crate) value: String,
    /// Whether it is the full-power default's value.
    pub(crate) is_default: bool,
}

/// The effective `[harness]` table: every key of
/// [`aterm_agent::supervise::config::KEYS`], in its order, its value, and
/// whether that is the full-power default's ([`SupervisorConfig::texts`]
/// against [`SupervisorConfig::default`]'s, the comparison
/// `SupervisorConfig::against_default` starts from) — with each agent's own
/// table's switch right after the master switch, as the SUPERVISION card
/// orders them ([`SupervisorConfig::agent_texts`]: `codex.enabled`), and,
/// last, a safe rule a retired key took away, under that key
/// ([`SupervisorConfig::withheld_texts`]: never a default). Every key the
/// policy reads, so what STATUS names as written is on this card.
#[must_use]
pub(crate) fn config_rows(cfg: &SupervisorConfig) -> Vec<HarnessConfigRow> {
    let full = SupervisorConfig::default();
    let row = |key: String, value: String, default: &str| HarnessConfigRow {
        key,
        is_default: value == default,
        value,
    };
    let agents = cfg
        .agent_texts()
        .into_iter()
        .zip(full.agent_texts())
        .map(|((key, value), (_, default))| row(key, value, &default));
    let mut rows: Vec<HarnessConfigRow> = cfg
        .texts()
        .into_iter()
        .zip(full.texts())
        .map(|((key, value), (_, default))| row(key.to_string(), value, &default))
        .collect();
    let after_master = rows
        .iter()
        .position(|row| row.key == "enabled")
        .map_or(0, |at| at + 1);
    rows.splice(after_master..after_master, agents);
    rows.extend(
        cfg.withheld_texts()
            .into_iter()
            .map(|(key, value)| HarnessConfigRow {
                key: key.to_string(),
                value,
                is_default: false,
            }),
    );
    rows
}

/// THE SETTINGS ▸ HARNESS SNAPSHOT: plain data, published whole on every
/// change ([`App::harness_state`]); the page renders its
/// [`Self::projection`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessState {
    /// Whether the host published it (a fresh Settings controller holds
    /// [`Self::unobserved`] until the first publish, and says it is reading).
    pub(crate) observed: bool,
    /// The policy as Settings shows it (`app_config::HarnessPolicy::words`).
    pub(crate) words: String,
    /// The window's harness posture.
    pub(crate) posture: HarnessPosture,
    /// Every live supervisable agent session, in local-id order.
    pub(crate) sessions: Vec<HarnessSession>,
    /// The ledgers' reading.
    pub(crate) stats: HarnessStatsSlot,
    /// The A/B totals at the publish.
    pub(crate) ab: AbTotals,
    /// The effective `[harness]` table.
    pub(crate) config: Vec<HarnessConfigRow>,
    /// The AI CLIs sub-page's snapshot ([`crate::ai_screen::AiState`]):
    /// published with its parent's, so the one fan-out serves both pages.
    pub(crate) ai: crate::ai_screen::AiState,
}

impl HarnessState {
    /// Nothing published yet: the page says it is reading, claims nothing.
    #[must_use]
    pub(crate) fn unobserved() -> Self {
        Self {
            observed: false,
            words: String::new(),
            posture: HarnessPosture::default(),
            sessions: Vec::new(),
            stats: HarnessStatsSlot::default(),
            ab: AbTotals::default(),
            config: Vec::new(),
            ai: crate::ai_screen::AiState::default(),
        }
    }

    /// What the page shows, derived once ([`HarnessProjection`]). Pure.
    #[must_use]
    pub(crate) fn projection(&self) -> HarnessProjection {
        HarnessProjection {
            observed: self.observed,
            status_rows: self.status_rows(),
            sessions: self.session_rows(),
            collecting: self.stats.collecting,
            did_rows: self.did_rows(),
            ab_rows: ab_rows(self.ab),
            config_rows: self.config.iter().map(config_fact).collect(),
            config_own: self
                .config
                .iter()
                .filter(|row| !row.is_default)
                .map(config_fact)
                .collect(),
            ai: self.ai.projection(self.observed),
        }
    }

    /// The STATUS card: the policy's words and, with any session open, how
    /// many of them the harness supervises. With none open the SESSIONS card
    /// under it says so, so this card does not say it again. The build's
    /// version is About's to say.
    fn status_rows(&self) -> Vec<(String, String)> {
        if !self.observed {
            return vec![("Supervision".to_string(), "reading\u{2026}".to_string())];
        }
        let open = self.sessions.len();
        if open == 0 {
            return vec![("Supervision".to_string(), self.words.clone())];
        }
        let supervised = self.sessions.iter().filter(|s| s.supervised).count();
        let sessions = match self.posture {
            HarnessPosture::On(_) | HarnessPosture::Restricted(_, _) => {
                format!("{supervised} of {open} supervised")
            }
            HarnessPosture::MasterOff => format!("{open} open \u{b7} none supervised"),
            HarnessPosture::Unhosted => {
                format!("{open} open \u{b7} no harness: this aterm has no control socket")
            }
        };
        vec![
            ("Supervision".to_string(), self.words.clone()),
            ("Sessions".to_string(), sessions),
        ]
    }

    /// The SESSIONS card's rows.
    fn session_rows(&self) -> Vec<HarnessSessionRow> {
        self.sessions
            .iter()
            .map(|session| {
                let tab = place_words(&session.place);
                let how = match session.view {
                    AutopilotView::MasterOff => "harness off in Settings".to_string(),
                    AutopilotView::Unhosted => "no harness here".to_string(),
                    view => format!(
                        "{} \u{b7} autopilot {} \u{b7} {}",
                        if session.supervised {
                            "supervised"
                        } else {
                            "not supervised"
                        },
                        view.wire(),
                        if session.word.is_some() {
                            "its own setting"
                        } else {
                            "the default"
                        }
                    ),
                };
                // Each word once: a title that already names the agent (its
                // product name standing in for none, or the agent's own
                // "✳ Claude Code") is not followed by it again.
                let detail = if names_program(&session.title, session.program) {
                    format!("{tab} \u{b7} {how}")
                } else {
                    format!("{tab} \u{b7} {} \u{b7} {how}", session.program)
                };
                HarnessSessionRow {
                    local_id: session.local_id,
                    title: session.title.clone(),
                    detail,
                    mode: session.view.mode(),
                    default: session.default,
                    view: session.view,
                    glass: session.glass,
                }
            })
            .collect()
    }

    /// The WHAT THE HARNESS DID card's facts: each count today and over 7
    /// days, the person's wait as hours and minutes, a window the ledgers'
    /// cut may have shortened, when the reading was taken, and — where a
    /// later reading failed — why the counts are that older reading's.
    fn did_rows(&self) -> Vec<(String, String)> {
        let row = |label: &str, value: String| (label.to_string(), value);
        let Some(report) = self.stats.report.as_deref() else {
            let words = if self.stats.collecting || !self.observed {
                "reading\u{2026}".to_string()
            } else if let Some(why) = &self.stats.unavailable {
                format!("could not be read: {why}")
            } else {
                "not checked yet \u{b7} press Refresh".to_string()
            };
            return vec![row("Activity", words)];
        };
        // A reading that failed after this one landed: the counts below are
        // the older reading's, and the page says why the newer is missing.
        let failed = self
            .stats
            .unavailable
            .as_ref()
            .map(|why| row("Last check", format!("failed: {why}")));
        let (today, week) = (&report.today, &report.week);
        let mut rows = Vec::new();
        if week.is_empty() {
            rows.push(row(
                "7 days",
                "nothing yet: no supervised session has acted".to_string(),
            ));
        } else {
            let both = |t: u64, w: u64| format!("{t} today \u{b7} {w} in 7 days");
            rows.extend([
                row("Approved", both(today.approved, week.approved)),
                row("Answered", both(today.answered, week.answered)),
                row("Continued", both(today.continued, week.continued)),
                row("Landed", both(today.landed, week.landed)),
                row("Resumed", both(today.resumed, week.resumed)),
                row("Relaunched", both(today.relaunched, week.relaunched)),
                row("Upgraded", both(today.upgraded, week.upgraded)),
                row("Escalated", both(today.escalated, week.escalated)),
                row(
                    "Waited on you",
                    format!(
                        "{} today \u{b7} {} in 7 days",
                        duration_words(today.human_wait_ms),
                        duration_words(week.human_wait_ms)
                    ),
                ),
            ]);
        }
        if today.cut || week.cut {
            rows.push(row(
                "Partial",
                format!(
                    "only the last {} actions are kept",
                    aterm_agent::supervise::approvals::KEEP_ROWS
                ),
            ));
        }
        rows.push(row(
            "Checked",
            read_at_words(report.collected_unix_ms, report.offset_s),
        ));
        rows.extend(failed);
        rows
    }
}

/// When a reading was taken, as a clock time — `at 14:05`, `at 14:05 UTC`
/// where the zone could not be read. Absolute, never "3 min ago": nothing
/// republishes the page while it stands open, so a relative time would go
/// stale on the glass.
#[must_use]
pub(crate) fn read_at_words(collected_ms: i64, offset_s: Option<i64>) -> String {
    let local_s = collected_ms
        .div_euclid(1000)
        .saturating_add(offset_s.unwrap_or(0));
    let of_day = local_s.rem_euclid(86_400);
    let zone = if offset_s.is_some() { "" } else { " UTC" };
    format!("at {:02}:{:02}{zone}", of_day / 3600, (of_day % 3600) / 60)
}

/// The EFFECT OF EACH AUTOPILOT card's facts: the waiting share under each Autopilot
/// state — Cruise, Land, Off, each "not measured yet" until it has time — and
/// the unsupervised time where there is any; then, once both have time,
/// Cruise's share beside Off's. Land is never in that sentence: a landing
/// ends waiting by design, so its share says how a landing went, not what
/// the harness changed. Never a "time saved": nothing measures one.
///
/// Once any state has time, a last row says what every share counts: an
/// agent that has FINISHED waits too — under Cruise after its final
/// statement, when the harness types nothing more; under Off when its
/// person has nothing more for it — and that wait is charged to its state
/// like any other. Nothing the window holds says a task is done (the
/// engine's done state lives in its worker, and the published agent word is
/// `idle` either way), so the card says it rather than leaving it out.
#[must_use]
pub(crate) fn ab_rows(ab: AbTotals) -> Vec<(String, String)> {
    let arm = |label: &str, totals: AbArmTotals| {
        let measured = totals.measured_ms();
        let words = if measured == 0 {
            "not measured yet".to_string()
        } else {
            format!(
                "waited for input {} of the time \u{b7} {} measured",
                share_words(totals.waiting_ms, measured),
                duration_words(measured)
            )
        };
        (label.to_string(), words)
    };
    let mut rows = vec![
        arm("Cruise", ab.cruise),
        arm("Land", ab.land),
        arm("Off", ab.off),
    ];
    if ab.unsupervised.measured_ms() > 0 {
        rows.push(arm("Unsupervised", ab.unsupervised));
    }
    let (cruise, off) = (ab.cruise.measured_ms(), ab.off.measured_ms());
    if cruise > 0 && off > 0 {
        rows.push((
            "This run".to_string(),
            format!(
                "agents waited for input {} of the time on Cruise vs {} with the harness off",
                share_words(ab.cruise.waiting_ms, cruise),
                share_words(ab.off.waiting_ms, off)
            ),
        ));
    }
    let measured = [ab.cruise, ab.land, ab.off, ab.unsupervised]
        .iter()
        .any(|totals| totals.measured_ms() > 0);
    if measured {
        rows.push((
            "Finished".to_string(),
            "an agent done with its work still waits for input; each share counts that wait"
                .to_string(),
        ));
    }
    rows
}

/// One key of the effective table as the card states it: the key, then its
/// value (`(none)` for an empty one) and whether it is the default or yours.
fn config_fact(row: &HarnessConfigRow) -> (String, String) {
    let value = if row.value.is_empty() {
        "(none)"
    } else {
        row.value.as_str()
    };
    let whose = if row.is_default { "default" } else { "yours" };
    (row.key.to_string(), format!("{value} \u{b7} {whose}"))
}

/// `part` of `whole` as a whole percent, never rounded to a claim it is not:
/// a share above nothing reads `<1%`, one short of all `>99%`.
#[must_use]
pub(crate) fn share_words(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0%".to_string();
    }
    let percent = (u128::from(part) * 100 + u128::from(whole) / 2) / u128::from(whole);
    if part > 0 && percent == 0 {
        "<1%".to_string()
    } else if part < whole && percent >= 100 {
        ">99%".to_string()
    } else {
        format!("{percent}%")
    }
}

/// A duration as hours and minutes (`2h 5m`, `45m`, `3h`); under a minute
/// but not nothing, `<1m`.
#[must_use]
pub(crate) fn duration_words(ms: u64) -> String {
    let minutes = ms / 60_000;
    if ms > 0 && minutes == 0 {
        return "<1m".to_string();
    }
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// A tab's place as a row leads with it: `in tab 2` → `Tab 2`, `in window 1,
/// tab 2` → `Window 1, tab 2`, and a tab no window shows `Its tab`.
fn place_words(place: &str) -> String {
    let bare = place.strip_prefix("in ").unwrap_or(place);
    let mut chars = bare.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What Settings ▸ Harness renders ([`HarnessState::projection`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessProjection {
    /// Whether the host has published.
    pub(crate) observed: bool,
    /// The STATUS card's facts.
    pub(crate) status_rows: Vec<(String, String)>,
    /// The SESSIONS card's rows (empty: no supervisable agent session).
    pub(crate) sessions: Vec<HarnessSessionRow>,
    /// A ledgers' reading is running (Refresh is busy).
    pub(crate) collecting: bool,
    /// The WHAT THE HARNESS DID card's facts.
    pub(crate) did_rows: Vec<(String, String)>,
    /// The EFFECT OF EACH AUTOPILOT card's facts.
    pub(crate) ab_rows: Vec<(String, String)>,
    /// The SETTINGS IN ATERM.TOML card's facts: each key, and its value with
    /// `default` or `yours` — the card's list with its disclosure open.
    pub(crate) config_rows: Vec<(String, String)>,
    /// Of [`Self::config_rows`], the keys the person wrote: the card's list
    /// with its disclosure closed (the rest are at their defaults).
    pub(crate) config_own: Vec<(String, String)>,
    /// What Settings ▸ Harness ▸ AI CLIs renders
    /// ([`crate::ai_screen::AiProjection`]).
    pub(crate) ai: crate::ai_screen::AiProjection,
}

/// One SESSIONS row: what it is and its three-way control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessSessionRow {
    /// The session a press names.
    pub(crate) local_id: u64,
    /// Its tab title.
    pub(crate) title: String,
    /// Its tab, its agent, and how the harness holds it.
    pub(crate) detail: String,
    /// The mode it runs under — the selected button — or `None` where the
    /// harness runs none (the master switch or its agent's switch off, no
    /// host).
    pub(crate) mode: Option<Autopilot>,
    /// Whether its Cruise, Land and Off act, and the default they are
    /// weighed against: its session's [`HarnessSession::default`] — `Some`
    /// only while the harness can steer this session. Where it carries
    /// (Cruise or Land) the default's button hands the session back to it
    /// ([`hands_back`], [`App::execute_native_harness`]); an Off default's
    /// button pins Off like any other press.
    pub(crate) default: Option<Autopilot>,
    /// Its effective Autopilot, the chip's view.
    pub(crate) view: AutopilotView,
    /// The harness's glass on its chip, which its state line is formatted
    /// from ([`Self::state_line`]).
    pub(crate) glass: ChipGlass,
}

impl HarnessSessionRow {
    /// The row's STATE LINE (§9.3.4), formatted from its glass under its view
    /// as the chip's face reads it ([`ChipFace::of`]): `lands at 18:00`
    /// (`lands Sat 16:00` on another day), `landed 17:42`, `done 17:42`,
    /// `quiet: no done reply after 2 nudges`, `quiet: a person's own
    /// exchange`, `held by /goal`, else the trail's newest act (`last: ✓
    /// approved 14:05`). Clock times only, never an age (§9.0). `None` at rest
    /// — v1's row.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the SESSIONS card draws it with the timer's disclosure (design §9.6.2); \
                      until then the face table's tests read it"
        )
    )]
    #[must_use]
    pub(crate) fn state_line(&self) -> Option<String> {
        use aterm_agent::supervise::policy::turn_end::LAND_NUDGES;
        let glass = &self.glass;
        let at = |word: &str| match glass.task_at {
            Some(clock) => format!("{word} {}", clock.as_str()),
            None => word.to_owned(),
        };
        let line = match ChipFace::of(self.view, glass) {
            ChipFace::CruiseUntil => glass.land_at.map(|land| match land.other_day {
                Some(day) => format!("lands {} {}", day.short(), land.clock.as_str()),
                None => format!("lands at {}", land.clock.as_str()),
            }),
            ChipFace::Landed => Some(at("landed")),
            ChipFace::Done => Some(at("done")),
            ChipFace::Spent => Some(format!("quiet: no done reply after {LAND_NUDGES} nudges")),
            ChipFace::Theirs => Some("quiet: a person's own exchange".to_owned()),
            ChipFace::Held => Some("held by /goal".to_owned()),
            ChipFace::Cruise
            | ChipFace::Land
            | ChipFace::Off
            | ChipFace::MasterOff
            | ChipFace::Unhosted => None,
        };
        line.or_else(|| {
            glass.last.map(|(act, clock)| {
                format!("last: {} {} {}", act.glyph(), act.word(), clock.as_str())
            })
        })
    }
}

/// Every action of a SESSIONS row starts with this: a press that writes a
/// session's Autopilot word. The control surface keeps it Owner-only, as it
/// keeps `meta set autopilot` (`control::escalated_op`, and the front
/// Settings surface on this page, `FrontControlSurface::HarnessSettings`):
/// Off to Cruise puts the harness's hands back on a session its owner took
/// them off (§2.1), which the Settings fine op does not reach.
pub(crate) const SESSION_ACTION_PREFIX: &str = "harness/session/";

/// A session's Autopilot button's action and key:
/// `harness/session/<local_id>/autopilot/<word>`.
#[must_use]
pub(crate) fn autopilot_action(local_id: u64, mode: Autopilot) -> String {
    format!(
        "{SESSION_ACTION_PREFIX}{local_id}/autopilot/{}",
        mode.word()
    )
}

/// [`autopilot_action`] read back: the session and the mode, or `None` for
/// any other action.
#[must_use]
pub(crate) fn parse_autopilot_action(action: &str) -> Option<(u64, Autopilot)> {
    let rest = action.strip_prefix(SESSION_ACTION_PREFIX)?;
    let (id, word) = rest.split_once("/autopilot/")?;
    Some((id.parse().ok()?, Autopilot::parse(word)?))
}

/// Whether a SESSIONS row's press of `pressed` under a default of `default`
/// hands the session back to the default (its own word cleared) rather than
/// writing `pressed` as its word ([`App::execute_native_harness`]): only a
/// press of the default's own mode, and only where that default CARRIES the
/// work (Cruise or Land). An Off press is always written — pinned, as the
/// footer's chip pins it — so following a default of Off never leaves a
/// session the owner turned off here open to a default moved up later
/// (fail-closed). The page's button that hands back says so. Pure.
#[must_use]
pub(crate) fn hands_back(pressed: Autopilot, default: Autopilot) -> bool {
    pressed == default && AutopilotView::from_mode(default).carries()
}

/// A session's name on Settings ▸ Harness: its tab's own name, `resolved`
/// as the tab strip resolves it (the person's title, else the terminal's
/// live one, else its folder: `app_tabs::resolved_terminal_title_rung`),
/// else its program's name — never blank, since the row's controls are
/// spoken by it ("Autopilot land for …") and a shell may set an empty title.
fn session_title(resolved: Option<&str>, program: &str) -> String {
    resolved
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(program)
        .to_string()
}

/// Whether a session's `title` is its agent's product name (`program`) and
/// nothing more, behind any status glyph the agent puts before it ("Claude
/// Code", "✳ Claude Code", "⠋ Codex"): the row's detail then does not name
/// the agent again. A title that only mentions it ("claude-docs") is not.
fn names_program(title: &str, program: &str) -> bool {
    title
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .eq_ignore_ascii_case(program)
}

// ---------------------------------------------------------------------------
// The App
// ---------------------------------------------------------------------------

impl App {
    /// THE A/B TOTALS for this aterm run (§4 item 5): each arm's busy and
    /// waiting time, the open stretches counted to now.
    #[must_use]
    pub(crate) fn harness_ab(&self) -> AbTotals {
        self.harness_screen
            .ab
            .totals_at(crate::turn_ledger::now_ms())
    }

    /// Session `local_id`'s A/B arm now ([`AbArm::of`] its effective
    /// Autopilot, this host's claim and anyone's). ONE read of its metadata
    /// (a leaf lock) at ONE instant: the word, and the live claim whose
    /// holder says both whether it is this host's and whether anyone's is
    /// live — two reads could straddle a claim's lapse or another's take.
    #[must_use]
    pub(crate) fn harness_ab_arm(&self, local_id: u64) -> AbArm {
        let Some(session) = self.pool.get(local_id) else {
            return AbArm::of(
                AutopilotView::of(self.harness_posture(), None),
                false,
                false,
            );
        };
        let posture = self.session_harness_posture(local_id);
        // The live claim's holder: `Some(true)` this host's, `Some(false)`
        // another's, `None` nobody's.
        let (word, ours) = {
            let meta = session.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let ours = meta
                .live_supervisor(crate::metrics::now_us())
                .map(|holder| holder == crate::autopilot::own_holder());
            (meta.autopilot_word(), ours)
        };
        AbArm::of(
            AutopilotView::of(posture, word),
            ours == Some(true),
            ours.is_some(),
        )
    }

    /// Session `local_id`'s published word moved and no pool borrow is live
    /// (an input stall's publish): [`HarnessScreen::note_phase`].
    pub(crate) fn note_harness_phase(&mut self, local_id: u64) {
        let arm = self.harness_ab_arm(local_id);
        let Some(ctx) = self.pool.get(local_id).map(|s| s.ctx.clone()) else {
            return;
        };
        self.harness_screen.note_phase(local_id, &ctx, arm);
    }

    /// Session `local_id`'s ARM may have moved (its Autopilot word written,
    /// its claim taken or given back): the stretch so far goes to the arm
    /// that was in force, and the session counts under its arm now from here.
    pub(crate) fn note_harness_arm(&mut self, local_id: u64) {
        let arm = self.harness_ab_arm(local_id);
        let Some(session) = self.pool.get(local_id) else {
            return;
        };
        // Read out under the timeline's guard and classified after it, as
        // [`HarnessScreen::note_phase`] does (meta before timeline, always).
        let (agent, word) = {
            let tl = session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let agent = tl.agent();
            (crate::harness_host::agent_of(agent), agent.word)
        };
        let phase = agent.and_then(|_| AbPhase::of(word));
        self.harness_screen
            .ab
            .observe(local_id, phase, arm, crate::turn_ledger::now_ms());
    }

    /// Every session's arm may have moved (the policy in force changed).
    pub(crate) fn note_harness_arms(&mut self) {
        let ids: Vec<u64> = self.pool.iter().map(|s| s.id).collect();
        for id in ids {
            self.note_harness_arm(id);
        }
    }

    /// Session `local_id` ended: its open stretch is settled.
    pub(crate) fn retire_harness_ab(&mut self, local_id: u64) {
        self.harness_screen
            .ab
            .retire(local_id, crate::turn_ledger::now_ms());
    }

    /// THE SNAPSHOT Settings ▸ Harness renders: the policy in force, every
    /// live supervisable agent session, the ledgers' last
    /// reading, the A/B totals now and the effective table. Leaf locks only,
    /// never nested (the registry's, then each session's timeline, then its
    /// metadata).
    #[must_use]
    pub(crate) fn harness_state(&self) -> HarnessState {
        let posture = self.harness_posture();
        let policy = self.harness_policy.get();
        HarnessState {
            observed: true,
            words: policy.words(),
            posture,
            sessions: self.harness_sessions(posture),
            stats: self.harness_screen.stats.clone(),
            ab: self.harness_ab(),
            config: config_rows(&policy.policy),
            ai: self.ai_state(),
        }
    }

    /// Every live session whose program is a supervisable agent
    /// (`harness_host::agent_of`), with its Autopilot. Live is in this App's
    /// pool, not only in the registry: a closed tab leaves the pool first
    /// (`detach_session_view`, whose retirement republishes the page) and the
    /// registry after (`retire_session_registration`), and a tab closed by
    /// hand is never `Exited` there, so the registry alone would list it as a
    /// ghost row until the next publish.
    fn harness_sessions(&self, posture: HarnessPosture) -> Vec<HarnessSession> {
        let handles = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        let holder = crate::harness_host::holder();
        let now_us = crate::metrics::now_us();
        let mut sessions = Vec::new();
        for handle in handles.iter().filter(|h| {
            h.state != crate::session_store::SessionState::Exited
                && self.pool.get(h.local_id).is_some()
        }) {
            let program = {
                let tl = handle
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                crate::harness_host::agent_of(tl.agent())
            };
            let session_posture = posture.for_agent(program);
            let Some(program) = program.filter(|p| p.supervisable()).map(|p| p.product()) else {
                continue;
            };
            let (person, word, supervised) = {
                let meta = handle.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                (
                    meta.presentation_value("title"),
                    meta.autopilot_word(),
                    meta.live_supervisor(now_us) == Some(holder.as_str()),
                )
            };
            // THE TAB'S NAME AS THE STRIP RESOLVES IT, from the terminal it
            // shows: the store's copy of the title is written only as the
            // front tab's window title is published, so it is stale for a
            // background tab, and with Settings in front. A try-lock, never
            // parked behind a PTY reader (`App::tab_titles`); contended, the
            // store's copy stands in for the terminal's rungs.
            let resolved = match person.filter(|person| !person.trim().is_empty()) {
                Some(person) => Some(person),
                None => self.pool.get(handle.local_id).and_then(|session| {
                    let rung = |t: &aterm_core::terminal::Terminal| {
                        use crate::cwd_native::ReportedCwd as _;
                        let cwd = t.native_working_directory();
                        crate::app_tabs::resolved_terminal_title_rung(
                            None,
                            t.title(),
                            cwd.as_deref(),
                        )
                    };
                    match session.term.try_lock() {
                        Ok(t) => rung(&t),
                        Err(std::sync::TryLockError::Poisoned(p)) => rung(&p.into_inner()),
                        Err(std::sync::TryLockError::WouldBlock) => {
                            crate::app_tabs::resolved_terminal_title_rung(None, &handle.title, None)
                        }
                    }
                }),
            };
            sessions.push(HarnessSession {
                local_id: handle.local_id,
                title: session_title(resolved.as_deref(), program),
                place: self.upgrade_place(handle.sid.as_str()),
                program,
                word,
                view: AutopilotView::of(session_posture, word),
                default: session_posture.default_mode(),
                supervised,
                glass: self.chip_glass(handle.local_id),
            });
        }
        sessions.sort_by_key(|s| s.local_id);
        sessions
    }

    /// Whether some Settings view is on the Harness route now.
    pub(crate) fn harness_page_shown(&self) -> bool {
        self.view_store.iter().any(|(view, link)| {
            matches!(link, crate::tab_model::View::Native(_)) && self.harness_page_in(view)
        })
    }

    /// Whether native view `view` is a Settings view on the Harness route —
    /// what the control surface's front-input fence asks of the front view
    /// (`App::front_control_surface`), since its session buttons are
    /// Owner-only ([`SESSION_ACTION_PREFIX`]).
    pub(crate) fn harness_page_in(&self, view: crate::tab_model::ViewId) -> bool {
        matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Harness
        )
    }

    /// Something the page shows moved (a host note, a claim, a session's
    /// end): republish while a view shows the page. Off the page nothing is
    /// built — the route's arrival publishes afresh.
    pub(crate) fn harness_screen_moved(&mut self) {
        if self.harness_page_shown() {
            self.publish_native_harness_state();
        }
    }

    /// Perform a Settings ▸ Harness request (`AppEffect::Harness`): read the
    /// ledgers again, or set a session's Autopilot through the one writer
    /// ([`Self::set_session_autopilot`], which republishes). A session's word
    /// is written only while the harness supervises that session — its own
    /// posture ([`Self::session_harness_posture`]: the master switch and its
    /// agent's own switch) — the page's buttons are inert otherwise, and a
    /// stale press is refused here too.
    ///
    /// **A carrying default's button hands the session back to it**
    /// ([`hands_back`]). Under a default of Cruise or Land, a press of that
    /// mode clears the session's own word (`meta unset autopilot`'s write)
    /// rather than pinning it, so the session follows the default again —
    /// and where it had no word, nothing is written at all. Any other press
    /// is written as the session's own word, Off ALWAYS: under a default of
    /// Off the Off button pins Off as the footer's chip does, so a default
    /// moved later (to Cruise, a deleted line) never puts the harness's hands
    /// back on a session its owner took them off here. The row says which it
    /// runs under ("its own setting" or "the default"), and the button that
    /// hands back says so; pinning a session to a carrying default's mode is
    /// `meta set autopilot` and the chip's.
    pub(crate) fn execute_native_harness(&mut self, request: crate::native_app::HarnessRequest) {
        match request {
            crate::native_app::HarnessRequest::Refresh => {
                self.start_harness_stats_refresh();
                self.publish_native_harness_state();
            }
            crate::native_app::HarnessRequest::Autopilot { session, word } => {
                // The session's own posture: its agent's switch Off steers
                // nothing there, whatever the window's.
                if let Some(default) = self.session_harness_posture(session).default_mode() {
                    self.set_session_autopilot(
                        session,
                        (!hands_back(word, default)).then_some(word),
                    );
                }
            }
            crate::native_app::HarnessRequest::AiLook => self.ask_ai_look(),
            // A press with no window named (`app_native` names the Settings
            // view's own): the tab opens in the front window.
            crate::native_app::HarnessRequest::AiSignIn { id } => {
                if let Some(window) = self.frontmost_window {
                    self.ai_sign_in(window, id);
                }
            }
        }
    }

    /// READ THE LEDGERS AGAIN, off the event loop: one named background
    /// thread folds today and the last 7 days ([`collect_harness_stats`]) and
    /// posts `Wake::HarnessStatsCollected`. A reading already running is not
    /// joined by another; a window without an event loop (a test App) reads
    /// nothing.
    pub(crate) fn start_harness_stats_refresh(&mut self) {
        if self.harness_screen.stats.collecting {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        self.harness_screen.sequence = self.harness_screen.sequence.wrapping_add(1);
        let sequence = self.harness_screen.sequence;
        let state_dir = aterm_types::dirs::state_dir();
        let harness_state = crate::harness_host::harness_state();
        let spawn = std::thread::Builder::new()
            .name("aterm-harness-stats".into())
            .spawn(move || {
                // Cosmetic to the terminal: it lands through the proxy when done.
                crate::qos::set_self(crate::qos::Role::Background);
                // A reader that panicked still answers, so Refresh is never
                // left busy for a reading that will not come.
                let result = std::panic::catch_unwind(|| {
                    collect_harness_stats(
                        state_dir.as_deref(),
                        harness_state.as_deref(),
                        unix_ms_now(),
                        crate::presence::local_offset_at,
                    )
                })
                .unwrap_or_else(|_| Err("the ledgers' reader failed".to_string()))
                .map(Box::new);
                let _ = proxy.send_event(crate::Wake::HarnessStatsCollected { sequence, result });
            });
        match spawn {
            Ok(_) => {
                self.harness_screen.stats.collecting = true;
                self.harness_screen.stats.unavailable = None;
            }
            Err(error) => {
                self.harness_screen.stats.unavailable =
                    Some(format!("the reader could not start: {error}"));
            }
        }
    }

    /// The ledgers' reading `sequence` landed: kept when it is the running
    /// one's, and published.
    pub(crate) fn finish_harness_stats(
        &mut self,
        sequence: u64,
        result: Result<Box<HarnessStatsReport>, String>,
    ) {
        if sequence != self.harness_screen.sequence || !self.harness_screen.stats.collecting {
            return;
        }
        let stats = &mut self.harness_screen.stats;
        stats.collecting = false;
        match result {
            Ok(report) => {
                stats.report = Some(report);
                stats.unavailable = None;
            }
            Err(why) => stats.unavailable = Some(why),
        }
        self.publish_native_harness_state();
    }
}

/// Unix milliseconds now (the wall clock: the ledgers' rows are stamped on
/// it).
fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "harness_screen_tests.rs"]
mod tests;
