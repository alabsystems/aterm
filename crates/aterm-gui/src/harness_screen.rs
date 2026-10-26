// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings ▸ Harness read projection (`docs/DESIGN-harness-autopilot-2026-10-15.md` §4) — the harness analogue of [`crate::packages_screen`].

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use aterm_agent::harness::stats::HarnessStats;
use aterm_agent::supervise::{Autopilot, SupervisorConfig};

use crate::App;
use crate::autopilot::{AutopilotView, HarnessPosture};
use crate::claude_lights::{ChipFace, ChipGlass, LandAtGlass};

const WEEK_MS: i64 = 7 * 86_400_000;

pub(crate) use crate::harness_ab::{AbArm, AbBook, AbCause, AbPhase, AbWeek, ab_rows};

/// One reading of the ledgers: today and the last 7 days, and when it was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessStatsReport {
    pub(crate) today: HarnessStats,
    pub(crate) week: HarnessStats,
    pub(crate) collected_unix_ms: i64,
    pub(crate) offset_s: Option<i64>,
    pub(crate) ab_others: Vec<crate::harness_ab::AbDay>,
    pub(crate) sibling_tabs: Vec<TabSpreadsheetRow>,
    pub(crate) remote_computers: Vec<TabComputer>,
    pub(crate) remote_tabs: Vec<TabSpreadsheetRow>,
    /// The build-slot minutes idle while a session's hold was ready, over the day the facts
    /// ring (`<state>/occupancy/facts.ring`) holds; `None` with no ring.
    pub(crate) idle_ready_min: Option<u64>,
    day_lines: BTreeMap<String, String>,
    decision_clocks: BTreeMap<i64, String>,
}

impl HarnessStatsReport {
    /// One immutable reading and its session-day text.
    pub(crate) fn new(
        today: HarnessStats,
        week: HarnessStats,
        collected_unix_ms: i64,
        offset_s: Option<i64>,
        offset_at: &dyn Fn(i64) -> Option<i64>,
    ) -> Self {
        let day_lines = today
            .by_session
            .as_ref()
            .into_iter()
            .flat_map(|shares| shares.iter())
            .filter_map(|(sid, tally)| {
                day_line_at(tally, offset_at).map(|line| (sid.clone(), line))
            })
            .collect();
        let decision_clocks = today
            .decided
            .as_ref()
            .into_iter()
            .flat_map(|decided| &decided.rows)
            .map(|decision| {
                (
                    decision.t_ms,
                    clock_words(decision.t_ms, offset_at(decision.t_ms.div_euclid(1000))),
                )
            })
            .collect();
        Self {
            today,
            week,
            collected_unix_ms,
            offset_s,
            ab_others: Vec::new(),
            sibling_tabs: Vec::new(),
            remote_computers: Vec::new(),
            remote_tabs: Vec::new(),
            idle_ready_min: None,
            day_lines,
            decision_clocks,
        }
    }
}

/// The unix ms of the local midnight that starts the day `now_ms` falls in.
#[must_use]
pub(crate) fn local_midnight_ms(now_ms: i64, offset_at: impl Fn(i64) -> Option<i64>) -> i64 {
    const DAY_MS: i64 = 86_400_000;
    const HOUR_S: i64 = 3_600;
    let ms = |offset_s: i64| offset_s.saturating_mul(1000);
    let Some(now_offset) = offset_at(now_ms.div_euclid(1000)) else {
        return now_ms - now_ms.rem_euclid(DAY_MS);
    };
    let at = |unix_s: i64| offset_at(unix_s).unwrap_or(now_offset);
    let local = now_ms.saturating_add(ms(now_offset));
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

/// READ THE LEDGERS (blocking file I/O: a worker's only), once: what the harness did since the local midnight (each session's share kept) and over the last 7 days, under aterm's state directory (`state_dir`, its `drive/` ledgers and journals) and the harness's own (`harness_state`, the live upgrade's ledger).
pub(crate) fn collect_harness_stats(
    state_dir: Option<&Path>,
    harness_state: Option<&Path>,
    now_ms: i64,
    offset_at: impl Fn(i64) -> Option<i64>,
) -> Result<HarnessStatsReport, String> {
    let (Some(state_dir), Some(harness_state)) = (state_dir, harness_state) else {
        return Err("no state directory resolves here".to_string());
    };
    let offset_s = offset_at(now_ms.div_euclid(1000));
    let (today, week) = aterm_agent::harness::stats::stats_windows(
        state_dir,
        harness_state,
        local_midnight_ms(now_ms, &offset_at),
        now_ms.saturating_sub(WEEK_MS),
    );
    let mut report = HarnessStatsReport::new(today, week, now_ms, offset_s, &offset_at);
    report.idle_ready_min = idle_ready_min_under(state_dir, now_ms);
    Ok(report)
}

/// The slot-minutes idle while ready over the day before `now_ms`, from the facts ring under
/// `state_dir` ([`aterm_agent::supervise::occupancy::ring`]); `None` where there is no ring.
pub(crate) fn idle_ready_min_under(state_dir: &Path, now_ms: i64) -> Option<u64> {
    use aterm_agent::supervise::occupancy::ring;
    let path = ring::ring_path(state_dir);
    let body = std::fs::read_to_string(path).ok()?;
    let rows = ring::rows_in(&body);
    let since = now_ms.saturating_sub(aterm_agent::supervise::occupancy::rules::DAY_MS);
    Some(ring::idle_ready_ms(&rows, since) / 60_000)
}

/// THE STATS THREAD'S READING (blocking file I/O: a worker's only): the ledgers ([`collect_harness_stats`]) and the A/B book's exchange (§10.5.3 C.2–3, `harness_ab_store::exchange`) — this process's book (`ab`, the ring the App copied and its today; `None` where it was never fed) written whole as `own` under `harness_state/ab/`, then every other process's days read into [`HarnessStatsReport::ab_others`].
pub(crate) fn read_harness_stats(
    state_dir: Option<&Path>,
    harness_state: Option<&Path>,
    ab: Option<&crate::harness_ab_store::AbSnapshot>,
    own: &str,
    now_ms: i64,
    offset_at: fn(i64) -> Option<i64>,
    fleet: &FleetSlot,
) -> Result<HarnessStatsReport, String> {
    let others = harness_state
        .map(|hs| crate::harness_ab_store::exchange(hs, own, ab, now_ms, offset_at))
        .unwrap_or_default();
    let read = collect_harness_stats(state_dir, harness_state, now_ms, offset_at);
    let ask = fleet.take();
    read.map(|mut report| {
        let (sibling_tabs, remote_computers, remote_tabs) = collect_fleet_and_sibling_tabs(ask);
        report.ab_others = others;
        report.sibling_tabs = sibling_tabs;
        report.remote_computers = remote_computers;
        report.remote_tabs = remote_tabs;
        report
    })
}

/// WHAT A RUNNING READING SWEEPS OF THE FLEET, RAISED WHILE IT RUNS: a reading begun for Settings ▸ Harness alone asks nothing of the fleet; a Tab Control page that comes while it reads the ledgers raises the ask ([`Self::raise`]) and the reading sweeps the fleet once they are read ([`Self::take`]), so that page's fleet lands with it — never a second reading of every ledger to fetch it.
#[derive(Debug, Default)]
pub(crate) struct FleetSlot(std::sync::atomic::AtomicU8);

impl FleetSlot {
    const FLEET: u8 = 1;
    const LOCAL_VITALS: u8 = 2;
    const TAKEN: u8 = 4;

    /// A slot asking `ask`.
    #[must_use]
    pub(crate) fn new(ask: FleetAsk) -> Self {
        Self(std::sync::atomic::AtomicU8::new(Self::bits(ask)))
    }

    fn bits(ask: FleetAsk) -> u8 {
        u8::from(ask.fleet) * Self::FLEET + u8::from(ask.local_vitals) * Self::LOCAL_VITALS
    }

    /// The reading, done with the ledgers: what it is asked now, closed to every later raise.
    pub(crate) fn take(&self) -> FleetAsk {
        let bits = self
            .0
            .fetch_or(Self::TAKEN, std::sync::atomic::Ordering::AcqRel);
        FleetAsk {
            fleet: bits & Self::FLEET != 0,
            local_vitals: bits & Self::LOCAL_VITALS != 0,
        }
    }

    /// Ask the running reading for `ask` too: whether it will sweep it (false where it was already taken).
    pub(crate) fn raise(&self, ask: FleetAsk) -> bool {
        self.0
            .fetch_or(Self::bits(ask), std::sync::atomic::Ordering::AcqRel)
            & Self::TAKEN
            == 0
    }
}

/// WHAT A READING ASKS OF THE FLEET ([`read_harness_stats`]): Settings ▸ Harness shows none of it, so its readings never wait on it (the post-landing check's perf-1: every Harness reading slept 30 ms and spawned `/usr/sbin/sysctl` twice for this Mac's vitals, and dialled the sibling sockets and the LAN, for a page that never draws them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FleetAsk {
    pub(crate) fleet: bool,
    pub(crate) local_vitals: bool,
}

/// WHAT OF THE FLEET THIS aterm MAY READ, decided once from what it is: an aterm of its account's
/// own reads the account's other aterm instances and the computers on its network; any other (a
/// private state folder, root, a home folder that is not its account's, a test build) is
/// isolated, and Tab Control shows it alone, with this Mac's vitals, and says why
/// ([`aterm_types::dirs::not_own_account`]). A scratch-home lab never dials the person's aterm
/// sockets or probes their network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FleetScope {
    /// Its user's own aterm.
    Account,
    /// Isolated, for the reason given (words that follow "not read here: ").
    Isolated(&'static str),
}

impl Default for FleetScope {
    fn default() -> Self {
        Self::Isolated(TEST_BUILD_ISOLATION)
    }
}

/// [`FleetScope::Isolated`]'s reason in a test build.
const TEST_BUILD_ISOLATION: &str = "it is a test build's aterm";

impl FleetScope {
    /// This process's scope, decided on first use and kept.
    pub(crate) fn of_this_process() -> Self {
        static SCOPE: std::sync::OnceLock<FleetScope> = std::sync::OnceLock::new();
        *SCOPE.get_or_init(|| {
            Self::decide(
                cfg!(test) || aterm_uds::test_binary::scratch_root().is_some(),
                aterm_types::dirs::not_own_account(),
            )
        })
    }

    /// The scope of a test build (`test_build`) or of an aterm that is not its account's own for
    /// the reason `not_own` gives ([`aterm_types::dirs::not_own_account`]).
    pub(crate) const fn decide(test_build: bool, not_own: Option<&'static str>) -> Self {
        match (test_build, not_own) {
            (true, _) => Self::Isolated(TEST_BUILD_ISOLATION),
            (false, Some(why)) => Self::Isolated(why),
            (false, None) => Self::Account,
        }
    }
}

/// The ledgers' reading as the page has it: the last report, whether a reading is running, and why none could be taken.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HarnessStatsSlot {
    pub(crate) report: Option<std::sync::Arc<HarnessStatsReport>>,
    pub(crate) collecting: bool,
    pub(crate) unavailable: Option<String>,
    pub(crate) stale: bool,
}

impl HarnessStatsSlot {
    /// Whether the newest reading's today is a day that has ended: it began before the local midnight that starts `now_ms`'s day ([`local_midnight_ms`], under `offset_at`).
    #[must_use]
    pub(crate) fn day_ended(&self, now_ms: i64, offset_at: impl Fn(i64) -> Option<i64>) -> bool {
        self.report
            .as_ref()
            .is_some_and(|report| report.today.since_ms < local_midnight_ms(now_ms, offset_at))
    }
}

/// What the App keeps for Settings ▸ Harness: the projection's revision, the A/B book and the ledgers' reading.
#[derive(Debug, Default)]
pub(crate) struct HarnessScreen {
    pub(crate) revision: u64,
    pub(crate) ab: AbBook,
    pub(crate) ab_writes: std::sync::Arc<crate::harness_ab_store::AbWrites>,
    pub(crate) stats: HarnessStatsSlot,
    sequence: u64,
    reread_at: Option<Instant>,
    owed_since: Option<Instant>,
    read_began: Option<Instant>,
    last_act: Option<Instant>,
    #[cfg(test)]
    pub(crate) readings_asked: u32,
    reading: FleetAsk,
    fleet_slot: std::sync::Arc<FleetSlot>,
    #[cfg(test)]
    pub(crate) last_fleet_ask: Option<FleetAsk>,
}

impl HarnessScreen {
    /// When the re-read an act owes is due, if one is owed.
    #[must_use]
    pub(crate) const fn reread_at(&self) -> Option<Instant> {
        self.reread_at
    }

    /// Whether the Live Fleet Monitor tab (`/tabs` / `/monitor`) is due for its next continuous 3-second fleet telemetry sweep at `now`.
    #[must_use]
    pub(crate) fn fleet_sweep_due(&self, now: Instant) -> bool {
        !self.stats.collecting
            && self.read_began.is_none_or(|began| {
                now.saturating_duration_since(began) >= std::time::Duration::from_secs(3)
            })
    }

    /// A reading of the ledgers began at `now`: the next re-read an act owes waits [`crate::harness_glass::REREAD`] from here.
    pub(crate) fn began_reading(&mut self, now: Instant) {
        self.stats.collecting = true;
        self.stats.unavailable = None;
        self.read_began = Some(now);
    }

    /// AN ACT AT `now`, told while the page is on screen, whether or not the chip's trail took it (a second box approved a second after the first is a repeat on the chip, and a row the page counts): WHAT THE HARNESS DID is stale and a re-read is owed ([`Self::owe_reread`]).
    pub(crate) fn note_act(&mut self, now: Instant) -> bool {
        self.last_act = Some(self.last_act.map_or(now, |last| last.max(now)));
        let marked = !self.stats.stale;
        self.stats.stale = true;
        self.owe_reread(now);
        marked
    }

    fn owe_reread(&mut self, act: Instant) {
        use crate::harness_glass::{DWELL, REREAD};
        let since = *self.owed_since.get_or_insert(act);
        let due = (act + DWELL).min(since + REREAD);
        let due = self.read_began.map_or(due, |began| due.max(began + REREAD));
        self.reread_at = Some(self.reread_at.map_or(due, |at| at.max(due)));
    }

    const fn take_reread(&mut self) {
        self.reread_at = None;
        self.owed_since = None;
    }

    fn covers_the_acts(&self) -> bool {
        match (self.read_began, self.last_act) {
            (_, None) => true,
            (Some(began), Some(last)) => began >= last + crate::harness_glass::DWELL,
            (None, Some(_)) => false,
        }
    }

    /// THE PAGE LEFT THE SCREEN: what the acts marked and owed goes — no re-read for a page nobody reads, no `Newer` left standing for its return, whose arrival reads the ledgers afresh.
    pub(crate) fn forget_acts(&mut self) {
        self.take_reread();
        self.last_act = None;
        self.stats.stale = false;
    }
}

impl HarnessScreen {
    /// Session `session`'s published agent word just moved (a sweep's publish, an input stall's): the stretch it closes goes to the bucket in force, and the new word's opens at the instant the word moved (its `changed_ms`, so no clock is read for it).
    pub(crate) fn note_phase(
        &mut self,
        session: u64,
        ctx: &crate::SessionCtx,
        arm: AbArm,
        glass: (bool, bool),
    ) {
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
        let cause = AbCause::of(arm, phase, glass.0, glass.1);
        self.ab_calendar();
        self.ab.observe(session, phase, cause, arm, at_ms);
    }

    /// THE A/B BOOK'S CALENDAR, read where the book is first fed: one wall read, with the ledger instant it pairs with ([`AbBook::set_calendar`]).
    pub(crate) fn ab_calendar(&mut self) {
        if !self.ab.has_calendar() {
            self.ab.set_calendar(crate::harness_ab::AbCalendar::read(
                crate::turn_ledger::now_ms(),
                crate::autopilot_timers::wall_ms(),
                crate::autopilot_timers::local_offset_at,
            ));
        }
    }
}

/// One live supervisable agent session as the page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessSession {
    pub(crate) local_id: u64,
    pub(crate) sid: String,
    pub(crate) title: String,
    pub(crate) place: String,
    pub(crate) program: &'static str,
    pub(crate) word: Option<Autopilot>,
    pub(crate) view: AutopilotView,
    pub(crate) default: Option<Autopilot>,
    pub(crate) supervised: bool,
    pub(crate) glass: ChipGlass,
    pub(crate) armed_at_ms: Option<i64>,
    pub(crate) set_by: Option<crate::autopilot::SetByWords>,
    pub(crate) need: Option<crate::needs::HarnessAsk>,
    pub(crate) hold: Option<crate::harness_glass::HeldGlass>,
    pub(crate) stopped: Option<aterm_agent::harness::acts::HeldBy>,
}

/// One `[harness]` key of the effective table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessConfigRow {
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) is_default: bool,
}

/// The effective `[harness]` table: every key of [`aterm_agent::supervise::config::KEYS`], in its order, its value, and whether that is the full-power default's ([`SupervisorConfig::texts`] against [`SupervisorConfig::default`]'s, the comparison `SupervisorConfig::against_default` starts from) — with each agent's own table's switch right after the master switch, as the SUPERVISION card orders them ([`SupervisorConfig::agent_texts`]: `codex.enabled`), and, last, a safe rule a retired key took away, under that key ([`SupervisorConfig::withheld_texts`]: never a default).
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

/// THE SETTINGS ▸ HARNESS SNAPSHOT: plain data, published whole on every change ([`App::harness_state`]); the page renders its [`Self::projection`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessState {
    pub(crate) observed: bool,
    pub(crate) words: String,
    pub(crate) posture: HarnessPosture,
    pub(crate) sessions: Vec<HarnessSession>,
    pub(crate) stats: HarnessStatsSlot,
    pub(crate) ab: AbWeek,
    pub(crate) config: Vec<HarnessConfigRow>,
    pub(crate) land_at_offer: Option<LandAtGlass>,
    pub(crate) ai: crate::ai_screen::AiState,
    pub(crate) tabs: TabControlState,
    pub(crate) decided_open: Vec<OpenSession>,
    pub(crate) today: i64,
}

/// One accessible computer in Settings ▸ Harness ▸ Tab Control (`/tabs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabComputer {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) os_version: String,
    pub(crate) status: String,
    pub(crate) vitals: String,
    pub(crate) instances: usize,
    pub(crate) tabs: usize,
    pub(crate) ai_clis: usize,
    pub(crate) active_ai: usize,
}

/// One row in the Tab Control Panel's sessions & tabs spreadsheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabSpreadsheetRow {
    pub(crate) key: String,
    pub(crate) computer: String,
    pub(crate) instance: String,
    pub(crate) place: String,
    pub(crate) sid: String,
    pub(crate) title: String,
    pub(crate) state: String,
    pub(crate) program: String,
    pub(crate) is_ai_cli: bool,
    pub(crate) ai_status: String,
    pub(crate) autopilot: String,
    pub(crate) human: String,
    pub(crate) vitals: String,
    pub(crate) show_local_id: Option<u64>,
}

/// Snapshot of all accessible computers, `aterm` instances, and tabs for Settings ▸ Harness ▸ Tab Control (`/tabs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabControlState {
    pub(crate) computers: Vec<TabComputer>,
    pub(crate) tabs: Vec<TabSpreadsheetRow>,
    /// What of the fleet this reading could look at.
    pub(crate) fleet: FleetScope,
}

/// Pure projection rendered by Settings ▸ Harness ▸ Tab Control (`/tabs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabControlProjection {
    pub(crate) observed: bool,
    pub(crate) collecting: bool,
    pub(crate) summary_rows: Vec<(String, String)>,
    pub(crate) computers: Vec<TabComputer>,
    pub(crate) tabs: Vec<TabSpreadsheetRow>,
}

impl TabControlState {
    /// Pure projection for Settings ▸ Harness ▸ Tab Control.
    #[must_use]
    pub(crate) fn projection(&self, observed: bool, collecting: bool) -> TabControlProjection {
        let summary_rows = if !observed {
            vec![("Fleet".to_string(), "reading\u{2026}".to_string())]
        } else {
            let partial = self.computers.iter().any(|c| c.id.starts_with("lan/"));
            let total_computers = self.computers.len();
            let online_computers = self
                .computers
                .iter()
                .filter(|c| c.status.starts_with("online"))
                .count();
            let total_instances: usize = self.computers.iter().map(|c| c.instances).sum::<usize>();
            let total_tabs = self.tabs.len();
            let ai_tabs = self.tabs.iter().filter(|t| t.is_ai_cli).count();
            let active_ai = self
                .tabs
                .iter()
                .filter(|t| {
                    t.is_ai_cli
                        && (t.ai_status.starts_with("working") || t.ai_status.starts_with("busy"))
                })
                .count();
            let mut rows = vec![(
                "Computers".to_string(),
                format!("{total_computers} known \u{b7} {online_computers} online"),
            )];
            if let FleetScope::Isolated(why) = self.fleet {
                rows.push((
                    "Fleet".to_string(),
                    format!("other computers and aterm instances are not read here: {why}"),
                ));
            }
            rows.extend([
                (
                    "Sessions".to_string(),
                    format!(
                        "{total_instances} aterm {} \u{b7} {total_tabs} open {}{}",
                        if total_instances == 1 {
                            "instance"
                        } else {
                            "instances"
                        },
                        if total_tabs == 1 { "tab" } else { "tabs" },
                        if partial { " observed" } else { "" }
                    ),
                ),
                (
                    "AI CLIs".to_string(),
                    if ai_tabs == 0 {
                        if partial {
                            "none observed"
                        } else {
                            "none running"
                        }
                        .to_string()
                    } else {
                        format!(
                            "{ai_tabs} {} \u{b7} {active_ai} active",
                            if partial { "observed" } else { "running" }
                        )
                    },
                ),
            ]);
            if self.computers.iter().any(|c| !c.vitals.is_empty()) {
                let kernel_or_ssh = self
                    .computers
                    .iter()
                    .filter(|c| c.vitals.starts_with("CPU "))
                    .count();
                let lan_rtt = self
                    .computers
                    .iter()
                    .filter(|c| c.vitals.starts_with("LAN "))
                    .count();
                let local_v = self
                    .computers
                    .first()
                    .map(|c| c.vitals.as_str())
                    .filter(|v| !v.is_empty())
                    .unwrap_or("sampling\u{2026}");
                let isolated = matches!(self.fleet, FleetScope::Isolated(_));
                rows.push((
                    "System Vitals".to_string(),
                    if isolated {
                        format!("this Mac: {local_v}")
                    } else {
                        format!(
                            "this Mac: {local_v} \u{2014} fleet telemetry: {kernel_or_ssh} full kernel/SSH \u{b7} {lan_rtt} live LAN RTT"
                        )
                    },
                ));
                rows.push((
                    "Live Monitor".to_string(),
                    if isolated {
                        "Continuous (1s local kernel)"
                    } else {
                        "Continuous (1s local kernel / 3s fleet sweep \u{b7} 30s mDNS cache)"
                    }
                    .to_string(),
                ));
            }
            rows
        };
        TabControlProjection {
            observed,
            collecting,
            summary_rows,
            computers: self.computers.clone(),
            tabs: self.tabs.clone(),
        }
    }
}

/// A live session a decision names that is no SESSIONS row ([`HarnessState::decided_open`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenSession {
    pub(crate) sid: String,
    pub(crate) local_id: u64,
    pub(crate) title: String,
}

/// One row of DECIDED FOR YOU (§10.4.1): a choice the harness made in the person's name today, worded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecidedRow {
    pub(crate) id: String,
    pub(crate) clock: String,
    pub(crate) sentence: String,
    pub(crate) show: Option<u64>,
    pub(crate) name: String,
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
            ab: AbWeek::default(),
            config: Vec::new(),
            land_at_offer: None,
            ai: crate::ai_screen::AiState::default(),
            tabs: TabControlState::default(),
            decided_open: Vec::new(),
            today: 0,
        }
    }

    /// What the page shows, derived once ([`HarnessProjection`]).
    #[must_use]
    pub(crate) fn projection(&self) -> HarnessProjection {
        HarnessProjection {
            observed: self.observed,
            status_rows: self.status_rows(),
            today: self.today_lines(),
            sessions: self.session_rows(),
            every: self.every_row(),
            collecting: self.stats.collecting,
            did_rows: self.did_rows(),
            ab_rows: ab_rows(&self.ab, self.today),
            ab_lead: crate::harness_ab::ab_lead(&self.ab),
            config_rows: self.config.iter().map(config_fact).collect(),
            config_own: self
                .config
                .iter()
                .filter(|row| !row.is_default)
                .map(config_fact)
                .collect(),
            land_at_offer: self.land_at_offer,
            ai: self.ai.projection(self.observed),
            tabs: self.tabs.projection(self.observed, self.stats.collecting),
            decided: self.decided_rows(),
            decided_total: self.decided_total(),
        }
    }

    /// Today's decisions (DECIDED FOR YOU, §10.4), kept or not; none before a reading.
    #[must_use]
    pub(crate) fn decided_total(&self) -> u64 {
        self.stats
            .report
            .as_deref()
            .and_then(|report| report.today.decided.as_ref())
            .map_or(0, |decided| decided.total)
    }

    /// DECIDED FOR YOU'S ROWS (§10.4.2, item 3), newest first: each of today's kept decisions at its clock time, keyed by its decision's id, in one sentence that names its session as SESSIONS does — its live title, with its place where another row shares the title — else by the name of the live session SESSIONS does not list ([`Self::decided_open`]), else `A closed session`; `Show tab` only while the session is open.
    #[must_use]
    pub(crate) fn decided_rows(&self) -> Vec<DecidedRow> {
        let Some(report) = self.stats.report.as_deref() else {
            return Vec::new();
        };
        let Some(decided) = report.today.decided.as_ref() else {
            return Vec::new();
        };
        let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        let mut rows: Vec<DecidedRow> = decided
            .rows
            .iter()
            .map(|decision| {
                let (name, show) = self.decision_session(&decision.sid);
                let id = decision.id();
                let same = seen.entry(id.clone()).or_insert(0);
                *same += 1;
                let id = if *same > 1 {
                    format!("{id}-{same}")
                } else {
                    id
                };
                DecidedRow {
                    id,
                    clock: report
                        .decision_clocks
                        .get(&decision.t_ms)
                        .cloned()
                        .unwrap_or_else(|| clock_words(decision.t_ms, None)),
                    sentence: decision_sentence(decision, &name),
                    show,
                    name,
                }
            })
            .collect();
        rows.reverse();
        rows
    }

    fn decision_session(&self, sid: &str) -> (String, Option<u64>) {
        if let Some(session) = self.sessions.iter().find(|s| s.sid == sid) {
            return (self.row_name(session), Some(session.local_id));
        }
        if let Some(open) = self.decided_open.iter().find(|s| s.sid == sid) {
            return (open.title.clone(), Some(open.local_id));
        }
        let name = if sid.is_empty() {
            "A session given no id"
        } else {
            "A closed session"
        };
        (name.to_string(), None)
    }

    fn row_name(&self, session: &HarnessSession) -> String {
        let shared = self
            .sessions
            .iter()
            .any(|other| other.local_id != session.local_id && other.title == session.title);
        if shared {
            format!(
                "{}, {}",
                session.title,
                session.place.strip_prefix("in ").unwrap_or(&session.place)
            )
        } else {
            session.title.clone()
        }
    }

    fn status_rows(&self) -> Vec<(String, String)> {
        if !self.observed {
            return vec![("Supervision".to_string(), "reading\u{2026}".to_string())];
        }
        let open = self.sessions.len();
        if open == 0 {
            return vec![("Supervision".to_string(), self.words.clone())];
        }
        let supervised = self.sessions.iter().filter(|s| s.supervised).count();
        let mut sessions = match self.posture {
            HarnessPosture::On(_)
            | HarnessPosture::Restricted(_, _)
            | HarnessPosture::AgentOff(_) => {
                format!("{supervised} of {open} supervised")
            }
            HarnessPosture::MasterOff => format!("{open} open \u{b7} none supervised"),
            HarnessPosture::Unhosted => {
                format!("{open} open \u{b7} no harness: this aterm has no control socket")
            }
        };
        let need_you = self.sessions.iter().filter(|s| s.need.is_some()).count();
        if need_you > 0 {
            sessions.push_str(&format!(
                "{}{need_you} need you",
                crate::native_ui::FACT_SEPARATOR
            ));
        }
        vec![
            ("Supervision".to_string(), self.words.clone()),
            ("Sessions".to_string(), sessions),
        ]
    }

    /// TODAY'S NUMBERS under STATUS's facts (§10.3.1): the first three nonzero families (`37 approved today · 12 kept going · 4 answered`, drawn in Hero type where it fits; landings lead where there are any, `2 landed today · 31 approved · 4 kept going`, §12.4.1, by [`Families`]' order), then the rest and the person's wait (`2 resumed · 1 land prompt · waited for you 4m`, plain, in [`NEEDED_YOU`]'s words; empty where there is none).
    #[must_use]
    pub(crate) fn today_lines(&self) -> Option<(String, String)> {
        if !self.observed {
            return None;
        }
        let Some(report) = self.stats.report.as_deref() else {
            return self
                .stats
                .collecting
                .then(|| (TODAY_READING.to_string(), String::new()));
        };
        let today = &report.today;
        let mut lead = families(Families::of_stats(today));
        let mut rest = lead.split_off(lead.len().min(3));
        if today.human_wait_ms > 0 {
            rest.push(waited_for_you_words(today.human_wait_ms));
        }
        match self.decided_total() {
            0 => {}
            1 => rest.push("1 choice made for you".to_string()),
            n => rest.push(format!("{n} choices made for you")),
        }
        if lead.is_empty() {
            lead = std::mem::take(&mut rest);
        }
        let Some(first) = lead.first_mut() else {
            return Some((TODAY_NOTHING_YET.to_string(), String::new()));
        };
        first.push_str(" today");
        Some((
            lead.join(crate::native_ui::FACT_SEPARATOR),
            rest.join(crate::native_ui::FACT_SEPARATOR),
        ))
    }

    fn session_rows(&self) -> Vec<HarnessSessionRow> {
        let days = self.stats.report.as_deref().map(|report| &report.day_lines);
        self.sessions
            .iter()
            .map(|session| {
                let tab = place_words(&session.place);
                let name = self.row_name(session);
                let today = days.and_then(|days| days.get(&session.sid)).cloned();
                let how = match session.view {
                    AutopilotView::MasterOff => "harness off in Settings".to_string(),
                    AutopilotView::AgentOff(agent) => {
                        crate::autopilot::agent_switch_off(agent).to_string()
                    }
                    AutopilotView::Unhosted => "no harness here".to_string(),
                    view => format!(
                        "{} \u{b7} autopilot {}",
                        if session.supervised {
                            "supervised"
                        } else {
                            "not supervised"
                        },
                        view.wire(),
                    ),
                };
                let detail = if names_program(&session.title, session.program) {
                    format!("{tab} \u{b7} {how}")
                } else {
                    format!("{tab} \u{b7} {} \u{b7} {how}", session.program)
                };
                HarnessSessionRow {
                    local_id: session.local_id,
                    title: session.title.clone(),
                    name,
                    detail,
                    day_line: today,
                    mode: session.view.mode(),
                    default: session.default,
                    view: session.view,
                    glass: session.glass,
                    armed_at_ms: session.armed_at_ms,
                    today: self.today,
                    setting: setting_line(session, self.today),
                    need: session.need.clone(),
                    hold: session.hold,
                    stopped: session.stopped,
                }
            })
            .collect()
    }

    /// SESSIONS' `Every session` ROW (design §11.5.1): where SESSIONS lists two sessions or more — whatever the switches say, so a switch pressed in SUPERVISION, under it, never adds or takes away a row and moves itself under the pointer — how they stand and what each press would move, by the press's own rule ([`EveryPress::moves`]).
    #[must_use]
    pub(crate) fn every_row(&self) -> Option<EveryRow> {
        use crate::autopilot_timers::TimerChoice;
        if self.sessions.len() < 2 {
            return None;
        }
        let count = |press: EveryPress| self.sessions.iter().filter(|s| press.moves(s)).count();
        let mode = |s: &HarnessSession| s.view.mode();
        let steered = || self.sessions.iter().filter(|s| s.default.is_some());
        let cruising = self
            .sessions
            .iter()
            .filter(|s| mode(s) == Some(Autopilot::Cruise));
        let first = self.sessions.first().map(|s| s.program);
        let follows = || steered().filter(|s| EveryPress::Follow.moves(s));
        let views = self
            .sessions
            .first()
            .map(|s| s.view)
            .filter(|view| self.sessions.iter().all(|s| s.view == *view));
        let why = match views {
            Some(AutopilotView::MasterOff) => "harness off in Settings",
            Some(AutopilotView::Unhosted) => "no harness here",
            Some(AutopilotView::AgentOff(agent)) => crate::autopilot::agent_switch_off(agent),
            _ if self
                .sessions
                .iter()
                .all(|s| matches!(s.view, AutopilotView::AgentOff(_))) =>
            {
                "each agent's own switch is off"
            }
            _ => "the harness steers none of them",
        };
        let wears =
            |face: ChipFace| move |s: &&HarnessSession| ChipFace::of(s.view, &s.glass) == face;
        Some(EveryRow {
            sessions: self.sessions.len(),
            steered: steered().count(),
            cruising: cruising.clone().count(),
            timed: cruising.filter(|s| s.armed_at_ms.is_some()).count(),
            landing: self.sessions.iter().filter(wears(ChipFace::Land)).count(),
            landed: self.sessions.iter().filter(wears(ChipFace::Landed)).count(),
            quiet: self.sessions.iter().filter(wears(ChipFace::Theirs)).count(),
            off: self.sessions.iter().filter(|s| !s.view.carries()).count(),
            pinned_off: steered().filter(|s| s.word == Some(Autopilot::Off)).count(),
            cruise: count(EveryPress::Mode(Autopilot::Cruise)),
            land: count(EveryPress::Mode(Autopilot::Land)),
            timer: count(EveryPress::Timer(TimerChoice::In(60))),
            follow: count(EveryPress::Follow),
            cruise_landing: steered().filter(wears(ChipFace::Land)).count(),
            cruise_landed: steered().filter(wears(ChipFace::Landed)).count(),
            cruise_quiet: steered().filter(wears(ChipFace::Theirs)).count(),
            cruise_timed: steered()
                .filter(|s| mode(s) == Some(Autopilot::Cruise) && s.armed_at_ms.is_some())
                .count(),
            follow_timed: follows().filter(|s| s.armed_at_ms.is_some()).count(),
            follow_to_off: follows()
                .filter(|s| s.default == Some(Autopilot::Off))
                .count(),
            why,
            product: first.filter(|p| self.sessions.iter().all(|s| s.program == *p)),
        })
    }

    /// WHAT THE HARNESS DID's rows (§10.3, §12.4.6, §14.6): each count with a week, today's and the week's (`Approved  31 today · 212 in 7 days`), with its part under it where the part's week has one (`no safe rule`, `past “done”`); [`NEEDED_YOU`]'s count and wait in one row ([`waited_for_you`]); every count whose week is 0 named in one last count row ([`NONE_IN_7_DAYS`]); then the waits at limits, the occupancy door's rows and the reading's own.
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
            let both = |t: u64, w: u64| in_windows(|d| format!("{t} {d}"), |d| format!("{w} {d}"));
            let waited = |t: u64, w: u64| {
                in_windows(
                    |d| format!("{} {d}", duration_words(t)),
                    |d| format!("{} {d}", duration_words(w)),
                )
            };
            // A count whose week is 0 is named once, in NONE_IN_7_DAYS' row, in the order the
            // rows stand. A part of the row over it (`no safe rule`, `past “done”`) stands only
            // where its week has one, and is never named there.
            let mut none = Vec::new();
            let mut count = |rows: &mut Vec<(String, String)>, words: CountWords, value| match value
            {
                Some(value) => rows.push(row(words.label, value)),
                None => none.push(words.many),
            };
            let counts = |t: u64, w: u64| (w > 0).then(|| both(t, w));
            count(&mut rows, APPROVED, counts(today.approved, week.approved));
            if week.unproven > 0 {
                rows.push(row(
                    UNPROVEN_OF_APPROVED,
                    both(today.unproven, week.unproven),
                ));
            }
            count(&mut rows, DECLINED, counts(today.declined, week.declined));
            count(&mut rows, ANSWERED, counts(today.answered, week.answered));
            count(
                &mut rows,
                KEPT_GOING,
                counts(today.continued, week.continued),
            );
            let (checked_today, checked_week) =
                (done_claims_checked(today), done_claims_checked(week));
            if checked_week > 0 {
                rows.push(row(
                    DONE_CHECKED_OF_KEPT_GOING,
                    both(checked_today, checked_week),
                ));
            }
            count(&mut rows, LANDED, counts(today.landed, week.landed));
            count(
                &mut rows,
                LAND_PROMPTS,
                counts(today.land_prompts, week.land_prompts),
            );
            count(&mut rows, RESUMED, counts(today.resumed, week.resumed));
            count(
                &mut rows,
                RELAUNCHED,
                counts(today.relaunched, week.relaunched),
            );
            count(&mut rows, UPGRADED, counts(today.upgraded, week.upgraded));
            count(
                &mut rows,
                NEEDED_YOU,
                (week.escalated > 0 || week.human_wait_ms > 0).then(|| waited_for_you(today, week)),
            );
            count(
                &mut rows,
                NEEDS_TOLD,
                counts(today.needs_told, week.needs_told),
            );
            if !none.is_empty() {
                rows.push(row(
                    NONE_IN_7_DAYS,
                    none.join(crate::native_ui::FACT_SEPARATOR),
                ));
            }
            rows.push(row(
                "Waited at limits",
                waited(today.limit_wait_ms, week.limit_wait_ms),
            ));
            // The occupancy door's fact lines, by kind: typed under `prompt`, would-be under
            // `observe`. Shown once a week holds any.
            let kinds: std::collections::BTreeSet<&String> = week
                .facts_told
                .keys()
                .chain(week.facts_would.keys())
                .collect();
            let of =
                |book: &BTreeMap<String, u64>, kind: &str| book.get(kind).copied().unwrap_or(0);
            for kind in kinds {
                rows.push(row(
                    &format!("Facts told ({kind})"),
                    both(of(&today.facts_told, kind), of(&week.facts_told, kind)),
                ));
                rows.push(row(
                    &format!("Facts it would tell ({kind})"),
                    both(of(&today.facts_would, kind), of(&week.facts_would, kind)),
                ));
            }
        }
        if let Some(min) = report.idle_ready_min {
            rows.push(row(
                "Idle while ready",
                format!("{min} slot-min in the last day"),
            ));
        }
        if self.stats.stale {
            rows.push(row(
                "Newer",
                "the harness has acted since this reading; reading again".to_string(),
            ));
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

/// When a reading was taken, as a clock time — `at 14:05`, `at 14:05 UTC` where the zone could not be read.
#[must_use]
pub(crate) fn read_at_words(collected_ms: i64, offset_s: Option<i64>) -> String {
    format!("at {}", clock_words(collected_ms, offset_s))
}

/// Unix ms `at_ms` as the local clock under `offset_s` (`14:05`; `14:05 UTC` where the zone could not be read).
#[must_use]
pub(crate) fn clock_words(at_ms: i64, offset_s: Option<i64>) -> String {
    let local_s = at_ms.div_euclid(1000).saturating_add(offset_s.unwrap_or(0));
    let of_day = local_s.rem_euclid(86_400);
    let zone = if offset_s.is_some() { "" } else { " UTC" };
    format!("{:02}:{:02}{zone}", of_day / 3600, (of_day % 3600) / 60)
}

/// A SESSIONS ROW'S SETTING LINE (design §11.7.1, §11.7.7): whose word `session` runs under — `its own setting, set 14:05 over aterm ctl` (who set it and when: clock times, never ages, the day named against `today`), `its own setting` where nothing is known of its setter, or `follows the default`.
pub(crate) fn setting_line(session: &HarnessSession, today: i64) -> String {
    match (session.word, &session.set_by) {
        (Some(_), Some(set_by)) => format!("its own setting, {}", set_by.line(Some(today))),
        (Some(_), None) => "its own setting".to_string(),
        (None, _) => "follows the default".to_string(),
    }
}

/// STATUS's today line on a day the harness has not acted ([`HarnessState::today_lines`]): a note, no count.
pub(crate) const TODAY_NOTHING_YET: &str = "nothing yet today";

/// STATUS's today line before the first reading: a note, no count.
pub(crate) const TODAY_READING: &str = "reading\u{2026}";

/// Whether STATUS's today `line` counts (`37 approved today · …`), as against one of its notes ([`TODAY_NOTHING_YET`], [`TODAY_READING`]): only a line of counts is drawn in Hero type ( owner-eye-7).
#[must_use]
pub(crate) fn today_counts(line: &str) -> bool {
    line != TODAY_NOTHING_YET && line != TODAY_READING
}

/// The families of acts a day says, in the order it says them: landed, approved, kept going, answered, resumed, land prompts, declined, relaunched, upgraded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Families([u64; 9]);

impl Families {
    /// A window's totals.
    #[must_use]
    pub(crate) const fn of_stats(s: &HarnessStats) -> Self {
        Self([
            s.landed,
            s.approved,
            s.continued,
            s.answered,
            s.resumed,
            s.land_prompts,
            s.declined,
            s.relaunched,
            s.upgraded,
        ])
    }

    /// A session's share of one.
    #[must_use]
    pub(crate) const fn of_tally(t: &aterm_agent::harness::stats::Tally) -> Self {
        Self([
            t.landed,
            t.approved,
            t.continued,
            t.answered,
            t.resumed,
            t.land_prompts,
            t.declined,
            t.relaunched,
            t.upgraded,
        ])
    }
}

/// ONE NAME FOR EACH COUNT (owner-eye-6): the words a count is said in on Settings ▸ Harness — STATUS's today line, each SESSIONS row's day line, and its label in WHAT THE HARNESS DID — so a count is named one way on the whole page, and, where the chip lights the act, in the chip's own word (`↻ kept going`, `✓ approved`: [`aterm_agent::harness::acts::Act::word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CountWords {
    pub(crate) label: &'static str,
    pub(crate) one: &'static str,
    pub(crate) many: &'static str,
}

impl CountWords {
    const fn act(label: &'static str, act: aterm_agent::harness::acts::Act) -> Self {
        Self {
            label,
            one: act.word(),
            many: act.word(),
        }
    }

    /// `n` in a line's words: `1 kept going`, `2 land prompts`.
    #[must_use]
    pub(crate) fn count(self, n: u64) -> String {
        format!("{n} {}", if n == 1 { self.one } else { self.many })
    }
}

/// Boxes the harness approved.
pub(crate) const APPROVED: CountWords =
    CountWords::act("Approved", aterm_agent::harness::acts::Act::Approved);
/// THE PRESSES NO SAFE RULE PROVED (the ledger's `unproven:` approvals): of the approved, those full power made where no safe rule proved the box.
pub(crate) const NO_SAFE_RULE_PROVED: &str = "no safe rule proved";
/// WHAT THE HARNESS DID's label for [`NO_SAFE_RULE_PROVED`]'s row, a part of the Approved row over it, its value the counts alone (§14.6): `no safe rule  2 today · 5 in 7 days`.
pub(crate) const UNPROVEN_OF_APPROVED: &str = "no safe rule";
/// Turns the harness continued (`Counter::Continued`).
pub(crate) const KEPT_GOING: CountWords =
    CountWords::act("Kept going", aterm_agent::harness::acts::Act::Continued);
/// WHAT THE HARNESS DID's label for THE DONE CLAIMS THE HARNESS CHECKED (§12.4.1, §14.6), a part of the Kept going row over it, its value the counts alone: of the turns it kept going, those that claimed done and were answered with Cruise's defend-and-find-more prompt, so `past “done”  3 today · 3 in 7 days` says the harness kept going past the agent's "done" three times.
pub(crate) const DONE_CHECKED_OF_KEPT_GOING: &str = "past \u{201c}done\u{201d}";
const DONE_CLAIM_RULES: [&str; 3] = {
    use aterm_agent::supervise::policy::turn_end::{
        RULE_DONE_CHECK, RULE_OPEN_ITEMS, RULE_UNCOMMITTED,
    };
    [RULE_DONE_CHECK, RULE_OPEN_ITEMS, RULE_UNCOMMITTED]
};

fn done_claims_checked(window: &HarnessStats) -> u64 {
    DONE_CLAIM_RULES
        .iter()
        .filter_map(|rule| window.by_rule.get(*rule))
        .fold(0, |sum, n| sum.saturating_add(*n))
}

/// Questions the harness answered.
pub(crate) const ANSWERED: CountWords =
    CountWords::act("Answered", aterm_agent::harness::acts::Act::Answered);
/// Work carried past a wall.
pub(crate) const RESUMED: CountWords =
    CountWords::act("Resumed", aterm_agent::harness::acts::Act::Resumed);
/// Land's instruction and its nudges typed: prompts, never landings (the chip's `⤓ landing` and `⤓ nudged` light the same rows).
pub(crate) const LAND_PROMPTS: CountWords = CountWords {
    label: "Land prompts",
    one: "land prompt",
    many: "land prompts",
};
/// Land's landings: a clean `Nothing left:` defense at a land act's yield, and no repository check found work left to land, each counted once at its first note ([`HarnessStats::landed`], §12.4.3 item 2) — the word the chip's `✓ landed` says.
pub(crate) const LANDED: CountWords = CountWords {
    label: "Landed",
    one: "landed",
    many: "landed",
};
/// Boxes the harness declined.
pub(crate) const DECLINED: CountWords =
    CountWords::act("Declined", aterm_agent::harness::acts::Act::Declined);
/// Agents the harness started again.
pub(crate) const RELAUNCHED: CountWords = CountWords {
    label: "Relaunched",
    one: "relaunched",
    many: "relaunched",
};
/// Agents the harness upgraded.
pub(crate) const UPGRADED: CountWords = CountWords {
    label: "Upgraded",
    one: "upgraded",
    many: "upgraded",
};
/// Times the harness stopped and asked the person (its escalations), and how long it waited on them: one row of WHAT THE HARNESS DID ([`waited_for_you`]), a day line's `waited for you once, 1m at 18:29`, STATUS's `waited for you 4m` (§14.6). Not `stopped`: §14.3's `stopped typing` is the hold, which is no escalation.
pub(crate) const NEEDED_YOU: CountWords = CountWords {
    label: "Waited for you",
    one: "waited for you",
    many: "waited for you",
};

/// Times a worker's `NEEDS YOU:` line went on the person's screen as an `owner=needs` ask while the work went on (Autopilot Cruise / Land): never a stop, so never one of [`NEEDED_YOU`]'s.
pub(crate) const NEEDS_TOLD: CountWords = CountWords {
    label: "Needs, carried on",
    one: "need, carried on",
    many: "needs, carried on",
};

/// WHAT THE HARNESS DID's last count row (§14.6): every count whose week is 0, each in its words, in the order the rows stand (`None in 7 days  declined · answered · resumed · relaunched · upgraded · waited for you`), so a week of nothing is one row, not a row of `0 today · 0 in 7 days` each.
pub(crate) const NONE_IN_7_DAYS: &str = "None in 7 days";

/// A WHAT THE HARNESS DID row's words over its two windows, today's then the week's, each window's part made from the window's name (`31 today · 212 in 7 days`, `2 today, 14m · 5 in 7 days, 1h 2m`).
#[must_use]
fn in_windows(today: impl FnOnce(&str) -> String, week: impl FnOnce(&str) -> String) -> String {
    [today("today"), week("in 7 days")].join(crate::native_ui::FACT_SEPARATOR)
}

/// [`NEEDED_YOU`]'s row: each window's count, then how long it waited where it waited (`2 today, 14m · 5 in 7 days, 1h 2m`). A wait inside a window on an ask raised before it counts too, so a window can read `0 today, 3m`.
#[must_use]
fn waited_for_you(today: &HarnessStats, week: &HarnessStats) -> String {
    let one = |s: &HarnessStats, window: &str| {
        if s.human_wait_ms > 0 {
            format!(
                "{} {window}, {}",
                s.escalated,
                duration_words(s.human_wait_ms)
            )
        } else {
            format!("{} {window}", s.escalated)
        }
    };
    in_windows(|d| one(today, d), |d| one(week, d))
}

/// How long the harness waited on the person, in [`NEEDED_YOU`]'s words: `waited for you 4m` (STATUS's today line, and a row's day where the wait stands on an ask raised before the day).
#[must_use]
fn waited_for_you_words(wait_ms: u64) -> String {
    format!("{} {}", NEEDED_YOU.one, duration_words(wait_ms))
}

/// How many times, in a day line's words: `once`, `twice`, `3 times`.
#[must_use]
fn times_words(n: u64) -> String {
    match n {
        1 => "once".to_string(),
        2 => "twice".to_string(),
        n => format!("{n} times"),
    }
}

/// [`Families`]' words, in its order.
pub(crate) const FAMILY_WORDS: [CountWords; 9] = [
    LANDED,
    APPROVED,
    KEPT_GOING,
    ANSWERED,
    RESUMED,
    LAND_PROMPTS,
    DECLINED,
    RELAUNCHED,
    UPGRADED,
];

/// The nonzero families, each in its words (`31 approved`, `1 kept going`, `1 land prompt`), in [`Families`]' order.
#[must_use]
pub(crate) fn families(counts: Families) -> Vec<String> {
    counts
        .0
        .iter()
        .zip(FAMILY_WORDS)
        .filter(|(n, _)| **n > 0)
        .map(|(n, words)| words.count(*n))
        .collect()
}

fn day_line_at(
    tally: &aterm_agent::harness::stats::Tally,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<String> {
    let offset = tally
        .last_escalated_ms
        .and_then(|at| offset_at(at.div_euclid(1000)));
    day_line(tally, offset)
}

/// A SESSIONS ROW'S DAY (§10.3.1) from its share of today's reading: `today:`, then `waited for you N times, <wait> at <the last escalation's clock>` where it stopped and asked the person ([`NEEDED_YOU`]), or `waited for you <wait>` where the day's wait stands on an ask raised before the day, then `N needs, carried on` where its worker's NEEDS YOU went on screen ([`NEEDS_TOLD`]), then at most three nonzero counts, its landings first ([`Families`]' order, §12.4.1), then `partial` where its own ledgers' cut may have shortened the day — `today: waited for you once, 1m at 18:29 · 2 needs, carried on · 1 landed · 31 approved` (§14.6), `today: 1 landed · 31 approved · 3 land prompts`.
#[must_use]
pub(crate) fn day_line(
    tally: &aterm_agent::harness::stats::Tally,
    offset_s: Option<i64>,
) -> Option<String> {
    let mut parts = Vec::new();
    if tally.escalated > 0 {
        let mut need = format!("{} {}", NEEDED_YOU.one, times_words(tally.escalated));
        if tally.human_wait_ms > 0 {
            need.push_str(&format!(", {}", duration_words(tally.human_wait_ms)));
        }
        if let Some(at) = tally.last_escalated_ms {
            need.push_str(&format!(" at {}", clock_words(at, offset_s)));
        }
        parts.push(need);
    } else if tally.human_wait_ms > 0 {
        // A wait today on an ask raised before the day: the time is still the person's, as the
        // page's Waited for you row and STATUS's today line say it (§14.6.1 departure 3).
        parts.push(waited_for_you_words(tally.human_wait_ms));
    }
    if tally.needs_told > 0 {
        parts.push(NEEDS_TOLD.count(tally.needs_told));
    }
    parts.extend(families(Families::of_tally(tally)).into_iter().take(3));
    if tally.cut {
        parts.push("partial".to_string());
    }
    (!parts.is_empty()).then(|| format!("today: {}", parts.join(crate::native_ui::FACT_SEPARATOR)))
}

fn config_fact(row: &HarnessConfigRow) -> (String, String) {
    let value = if row.value.is_empty() {
        "(none)"
    } else {
        row.value.as_str()
    };
    let whose = if row.is_default { "default" } else { "yours" };
    (row.key.to_string(), format!("{value} \u{b7} {whose}"))
}

/// `part` of `whole` as a whole percent, never rounded to a claim it is not: a share above nothing reads `<1%`, one short of all `>99%`.
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

/// A duration as hours and minutes (`2h 5m`, `45m`, `3h`); under a minute but not nothing, `<1m`.
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

/// A DECISION IN ONE SENTENCE (§10.4.1), its session named `name`, the
/// agent's words in quotes:
///
/// * a dialog: `<name> asked “<question>” → the harness chose <option>:
///   <why>`;
/// * a prose question: `<name> asked “<question>” → <the answer>` (`said`
///   where the words are the agent's last row at the turn's end);
/// * an unproven press: `<name>: pressed Yes on <command head> ×N under
///   Approve permission boxes: All; no safe rule proved it` — or, where full
///   power chose another of the box's options, `chose “<option>” on <command
///   head>` — the switch that pressed it named, never "unsafe";
/// * a refusal: `<name>: declined <command head>: <the reason it gave>`;
/// * a limit reset kept: `<name>: kept its limit reset unspent: <why>`.
#[must_use]
pub(crate) fn decision_sentence(
    decision: &aterm_agent::harness::stats::Decision,
    name: &str,
) -> String {
    use aterm_agent::harness::stats::DecisionKind;
    let quoted = |text: &str| format!("\u{201c}{text}\u{201d}");
    let boxed = |head: &str| {
        if head.is_empty() {
            "a box".to_string()
        } else {
            head.to_string()
        }
    };
    match decision.kind {
        DecisionKind::Chose => {
            let asked = if decision.asked.is_empty() {
                "asked a question".to_string()
            } else {
                format!("asked {}", quoted(&decision.asked))
            };
            let chose = if decision.did.is_empty() {
                "an answer"
            } else {
                &decision.did
            };
            let why = decision
                .why
                .as_deref()
                .map_or_else(String::new, |why| format!(": {why}"));
            format!("{name} {asked} \u{2192} the harness chose {chose}{why}")
        }
        DecisionKind::Answered => {
            let asked = match (decision.asked.is_empty(), decision.said) {
                (true, _) => "asked a question in prose".to_string(),
                (false, true) => format!("said {}", quoted(&decision.asked)),
                (false, false) => format!("asked {}", quoted(&decision.asked)),
            };
            format!("{name} {asked} \u{2192} {}", decision.did)
        }
        DecisionKind::Unproven => {
            let times = if decision.times > 1 {
                format!(" \u{d7}{}", decision.times)
            } else {
                String::new()
            };
            let switch = decision
                .why
                .as_deref()
                .unwrap_or("Approve permission boxes: All");
            let pressed = match &*decision.did {
                "" | "Yes" => "pressed Yes".to_string(),
                option => format!("chose {}", quoted(option)),
            };
            format!(
                "{name}: {pressed} on {}{times} under {switch}; {NO_SAFE_RULE_PROVED} it",
                boxed(&decision.asked)
            )
        }
        DecisionKind::Declined => match decision.why.as_deref() {
            Some(why) => format!("{name}: declined {}: {why}", boxed(&decision.asked)),
            None => format!("{name}: declined {}", boxed(&decision.asked)),
        },
        DecisionKind::KeptReset => match decision.why.as_deref() {
            Some(why) => format!("{name}: kept its limit reset unspent: {why}"),
            None => format!("{name}: kept its limit reset unspent"),
        },
    }
}

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
    pub(crate) observed: bool,
    pub(crate) status_rows: Vec<(String, String)>,
    pub(crate) today: Option<(String, String)>,
    pub(crate) sessions: Vec<HarnessSessionRow>,
    pub(crate) every: Option<EveryRow>,
    pub(crate) collecting: bool,
    pub(crate) did_rows: Vec<(String, String)>,
    pub(crate) ab_rows: Vec<(String, String)>,
    pub(crate) ab_lead: Option<crate::harness_ab::AbLead>,
    pub(crate) config_rows: Vec<(String, String)>,
    pub(crate) config_own: Vec<(String, String)>,
    pub(crate) land_at_offer: Option<LandAtGlass>,
    pub(crate) ai: crate::ai_screen::AiProjection,
    pub(crate) tabs: TabControlProjection,
    pub(crate) decided: Vec<DecidedRow>,
    pub(crate) decided_total: u64,
}

/// One SESSIONS row: what it is and its three-way control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessSessionRow {
    pub(crate) local_id: u64,
    pub(crate) title: String,
    pub(crate) name: String,
    pub(crate) detail: String,
    pub(crate) day_line: Option<String>,
    pub(crate) mode: Option<Autopilot>,
    pub(crate) default: Option<Autopilot>,
    pub(crate) view: AutopilotView,
    pub(crate) glass: ChipGlass,
    pub(crate) armed_at_ms: Option<i64>,
    pub(crate) today: i64,
    pub(crate) setting: String,
    pub(crate) need: Option<crate::needs::HarnessAsk>,
    pub(crate) hold: Option<crate::harness_glass::HeldGlass>,
    pub(crate) stopped: Option<aterm_agent::harness::acts::HeldBy>,
}

impl HarnessSessionRow {
    /// Where its session is — its detail's lead (`Tab 1, left pane`, `Window 2, tab 1, right pane`): what tells two agents of one tab apart, so the page lays the row out where it is drawn whole (`native_settings::harness_page`'s `session_rows`).
    #[must_use]
    pub(crate) fn place(&self) -> &str {
        self.detail
            .split_once(crate::native_ui::FACT_SEPARATOR)
            .map_or(self.detail.as_str(), |(lead, _)| lead)
    }

    /// The row's STATE LINE (§9.3.4, §10.2.3 item 2), led by a harness's need where one stands (`needs you: … (+1 more); carrying on · `, §12.2.1), formatted from its glass under its view as the chip's face reads it ([`ChipFace::of`]): `lands at 18:00` (`lands Sat 16:00` on another day), under Land `landed 17:42` and `quiet: a person's own exchange`; else the loop's pause after short turns (`pausing until 18:41 (3 short turns in a row)`, §10.5.1); else the trail's newest act (`last: ✓ approved 14:05`).
    #[must_use]
    pub(crate) fn state_line(&self) -> Option<String> {
        let glass = &self.glass;
        let today = Some(self.today);
        let at = |word: &str| match glass.task_at {
            Some(stamp) => format!("{word} {}", stamp.words(today)),
            None => word.to_owned(),
        };
        let lands = |land: LandAtGlass| match land.other_day {
            Some(day) => format!("lands {} {}", day.short(), land.clock.as_str()),
            None => format!("lands at {}", land.clock.as_str()),
        };
        let face = ChipFace::of(self.view, glass);
        let hold = self.hold.filter(|_| face == ChipFace::Land);
        let stop = self
            .stopped
            .filter(|_| face.carries())
            .map(|arm| crate::harness_glass::stop_words(arm, crate::harness_glass::HoldForm::Line));
        // A stopped tab is not pausing: the pause it waited out before the hold has lapsed.
        let pause = glass.pause.filter(|_| stop.is_none());
        let line = match face {
            ChipFace::CruiseUntil => glass.land_at.map(lands),
            ChipFace::Landed => Some(at("landed")),
            ChipFace::Theirs => Some("quiet: a person's own exchange".to_owned()),
            ChipFace::Land => hold
                .and_then(|hold| {
                    crate::harness_glass::hold_words(
                        &hold,
                        self.need.as_ref(),
                        crate::harness_glass::HoldForm::Line,
                        today,
                    )
                })
                .map(|held| match pause {
                    Some(pause) => format!(
                        "{held}{}{}",
                        crate::native_ui::FACT_SEPARATOR,
                        pause.state_words(today)
                    ),
                    None => held,
                }),
            ChipFace::Cruise
            | ChipFace::Off
            | ChipFace::MasterOff
            | ChipFace::AgentOff(_)
            | ChipFace::Unhosted => None,
        };
        let need = self.need.as_ref().filter(|_| {
            !hold.is_some_and(|hold| {
                hold.read() && hold.held.holds(aterm_agent::harness::acts::HoldLeg::Needs)
            })
        });
        let rest = line
            .or_else(|| pause.map(|p| p.state_words(today)))
            .or_else(|| {
                glass.last.map(|(act, stamp)| {
                    format!(
                        "last: {} {} {}",
                        act.glyph(),
                        act.word(),
                        stamp.words(today)
                    )
                })
            });
        let sep = crate::native_ui::FACT_SEPARATOR;
        [stop, need.map(need_head), rest]
            .into_iter()
            .flatten()
            .reduce(|line, more| format!("{line}{sep}{more}"))
    }
}

/// A state line's head where a harness's need stands (§12.2.1): `needs you: <the need> (+1 more); carrying on`.
pub(crate) fn need_head(ask: &crate::needs::HarnessAsk) -> String {
    format!("needs you: {}{}; carrying on", ask.first, ask.more_words())
}

/// THE LANDING (§10.3.3, item 8): the host opens Settings ▸ Harness with this action ([`App::open_harness_settings`]), its value a [`HarnessFocus`]'s words, and the page's list lands on that row (drawn selected) or card.
pub(crate) const HARNESS_LAND: &str = "harness/land";

/// WHERE SETTINGS ▸ HARNESS LANDS as it opens (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.3.3, item 8): its top (View ▸ Coding Agent Harness…, the palette), a session's row (the Autopilot menu's `Harness settings…`), or SUPERVISION (an `⊘ off` chip: the switch that turned it off is there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HarnessFocus {
    /// The page's top.
    #[default]
    Top,
    /// The SESSIONS row of the session with this local id.
    Session(u64),
    /// The SUPERVISION card.
    Supervision,
}

impl HarnessFocus {
    /// Its words on the page's landing action (`top`, `session/7`, `supervision`).
    #[must_use]
    pub(crate) fn wire(self) -> String {
        match self {
            Self::Top => "top".to_string(),
            Self::Session(id) => format!("session/{id}"),
            Self::Supervision => "supervision".to_string(),
        }
    }

    /// [`Self::wire`] read back.
    #[must_use]
    pub(crate) fn parse(words: &str) -> Option<Self> {
        match words {
            "top" => Some(Self::Top),
            "supervision" => Some(Self::Supervision),
            _ => words
                .strip_prefix("session/")?
                .parse()
                .ok()
                .map(Self::Session),
        }
    }
}

/// Every action of a SESSIONS row starts with this: a press that writes a session's Autopilot word.
pub(crate) const SESSION_ACTION_PREFIX: &str = "harness/session/";

/// A session's Autopilot button's action and key: `harness/session/<local_id>/autopilot/<word>`.
#[must_use]
pub(crate) fn autopilot_action(local_id: u64, mode: Autopilot) -> String {
    format!(
        "{SESSION_ACTION_PREFIX}{local_id}/autopilot/{}",
        mode.word()
    )
}

/// [`autopilot_action`] read back: the session and the mode, or `None` for any other action.
#[must_use]
pub(crate) fn parse_autopilot_action(action: &str) -> Option<(u64, Autopilot)> {
    let rest = action.strip_prefix(SESSION_ACTION_PREFIX)?;
    let (id, word) = rest.split_once("/autopilot/")?;
    Some((id.parse().ok()?, Autopilot::parse(word)?))
}

/// Whether a SESSIONS row's press of `pressed` under a default of `default` hands the session back to the default (its own word cleared) rather than writing `pressed` as its word ([`App::execute_native_harness`]): only a press of the default's own mode, and only where that default CARRIES the work (Cruise or Land).
#[must_use]
pub(crate) fn hands_back(pressed: Autopilot, default: Autopilot) -> bool {
    pressed == default && AutopilotView::from_mode(default).carries()
}

/// THE PAGE'S ONE RULE FOR A MODE PRESS (design §11.5.3, item 3): the word a press of `pressed` writes under a default of `default` — `None`, the session's own word cleared, where the press hands the session back ([`hands_back`]), else `pressed`.
#[must_use]
pub(crate) fn page_word(pressed: Autopilot, default: Autopilot) -> Option<Autopilot> {
    (!hands_back(pressed, default)).then_some(pressed)
}

/// The `Every session` row's id in its keys: `harness/session/all/…`, under [`SESSION_ACTION_PREFIX`] on purpose, so the prefix's one Owner fence covers them, fail-closed (design §11.5.2).
pub(crate) const EVERY_ID: &str = "all";

/// A PRESS ON SESSIONS' `Every session` ROW (design §11.5.1): one choice for every session the harness can steer, each moved by [`Self::moves`] and written as [`Self::write`] says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EveryPress {
    /// Cruise or Land.
    Mode(Autopilot),
    /// A timed Cruise, `1 hour`, `2 hours`, `4 hours` or `at 18:00`, resolved ONCE at the press, so every session it arms lands at one instant.
    Timer(crate::autopilot_timers::TimerChoice),
    /// Follow the default: every session's own word cleared but a pinned Off.
    Follow,
}

impl EveryPress {
    /// Every press the row draws, in its order: Cruise, Land, the Timer's choices (`at` only with `[harness] land_at` set, the page's to say), Follow the default.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 7] = {
        use crate::autopilot_timers::TimerChoice;
        [
            Self::Mode(Autopilot::Cruise),
            Self::Mode(Autopilot::Land),
            Self::Timer(TimerChoice::In(60)),
            Self::Timer(TimerChoice::In(120)),
            Self::Timer(TimerChoice::In(240)),
            Self::Timer(TimerChoice::At),
            Self::Follow,
        ]
    };

    /// Whether this press moves `session` (design §11.5.1's table), a session the harness can steer ([`HarnessSession::default`]) by the mode it runs under: Land moves every Cruise, timed or not; Cruise moves every Land and every timed Cruise (a hand write drops the timer); a timer moves every Cruise; Follow moves every own word but Off.
    #[must_use]
    pub(crate) fn moves(self, session: &HarnessSession) -> bool {
        use crate::autopilot_timers::TimerChoice;
        if session.default.is_none() {
            return false;
        }
        let mode = session.view.mode();
        match self {
            Self::Mode(Autopilot::Land) | Self::Timer(TimerChoice::In(_) | TimerChoice::At) => {
                mode == Some(Autopilot::Cruise)
            }
            Self::Mode(Autopilot::Cruise) => {
                mode == Some(Autopilot::Land)
                    || (mode == Some(Autopilot::Cruise) && session.armed_at_ms.is_some())
            }
            Self::Mode(Autopilot::Off) | Self::Timer(TimerChoice::Cancel) => false,
            Self::Follow => matches!(session.word, Some(Autopilot::Cruise | Autopilot::Land)),
        }
    }

    /// The pair this press writes on a session it moves, under the session's `default`, `(word, land_at)`: a mode by the page's one rule ([`page_word`]), with no timer; a timed Cruise at `at`, the instant resolved once for the press; Follow, no word.
    #[must_use]
    pub(crate) fn write(
        self,
        default: Autopilot,
        at: Option<i64>,
    ) -> (Option<Autopilot>, Option<i64>) {
        match self {
            Self::Mode(mode) => (page_word(mode, default), None),
            Self::Timer(_) => (Some(Autopilot::Cruise), at),
            Self::Follow => (None, None),
        }
    }
}

/// An `Every session` button's action and key (design §11.5.2): `harness/session/all/autopilot/<cruise|land>`, `harness/session/all/land-in/<min>`, `harness/session/all/land-at` and `harness/session/all/follow`.
#[must_use]
pub(crate) fn every_action(press: EveryPress) -> String {
    use crate::autopilot_timers::TimerChoice;
    let every = format!("{SESSION_ACTION_PREFIX}{EVERY_ID}");
    match press {
        EveryPress::Mode(mode) => format!("{every}/autopilot/{}", mode.word()),
        EveryPress::Timer(TimerChoice::In(minutes)) => format!("{every}/land-in/{minutes}"),
        EveryPress::Timer(TimerChoice::At) => format!("{every}/land-at"),
        EveryPress::Timer(TimerChoice::Cancel) => format!("{every}/land-cancel"),
        EveryPress::Follow => format!("{every}/follow"),
    }
}

/// [`every_action`] read back: the press, or `None` for any other action — a session's own, an Off (no bulk Off), a cancel (no bulk cancel), a duration the page does not offer.
#[must_use]
pub(crate) fn parse_every_action(action: &str) -> Option<EveryPress> {
    use crate::autopilot_timers::TimerChoice;
    let rest = action
        .strip_prefix(SESSION_ACTION_PREFIX)?
        .strip_prefix(EVERY_ID)?
        .strip_prefix('/')?;
    match rest {
        "follow" => Some(EveryPress::Follow),
        "land-at" => Some(EveryPress::Timer(TimerChoice::At)),
        _ => {
            if let Some(word) = rest.strip_prefix("autopilot/") {
                return match Autopilot::parse(word)? {
                    Autopilot::Off => None,
                    mode if mode.word() == word => Some(EveryPress::Mode(mode)),
                    _ => None,
                };
            }
            let minutes: u32 = rest.strip_prefix("land-in/")?.parse().ok()?;
            crate::autopilot_menu::LAND_IN_MINUTES
                .contains(&minutes)
                .then_some(EveryPress::Timer(TimerChoice::In(minutes)))
        }
    }
}

/// SESSIONS' `Every session` ROW as the page draws it (design §11.5.1): how the sessions SESSIONS lists stand, and how many each press moves, read by [`EveryPress::moves`] — the rule the press itself runs ([`App::press_autopilot_every`]), so the row never counts what its press would not write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EveryRow {
    pub(crate) sessions: usize,
    pub(crate) steered: usize,
    pub(crate) cruising: usize,
    pub(crate) timed: usize,
    pub(crate) landing: usize,
    pub(crate) landed: usize,
    pub(crate) quiet: usize,
    pub(crate) off: usize,
    pub(crate) pinned_off: usize,
    pub(crate) cruise: usize,
    pub(crate) land: usize,
    pub(crate) timer: usize,
    pub(crate) follow: usize,
    pub(crate) cruise_landing: usize,
    pub(crate) cruise_landed: usize,
    pub(crate) cruise_quiet: usize,
    pub(crate) cruise_timed: usize,
    pub(crate) follow_timed: usize,
    pub(crate) follow_to_off: usize,
    pub(crate) why: &'static str,
    pub(crate) product: Option<&'static str>,
}

/// `1 session`, `3 sessions`.
pub(crate) fn sessions_words(n: usize) -> String {
    if n == 1 {
        "1 session".to_string()
    } else {
        format!("{n} sessions")
    }
}

fn under_land(landing: usize, landed: usize, quiet: usize) -> Vec<String> {
    [(landing, "landing"), (landed, "landed"), (quiet, "quiet")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect()
}

impl EveryRow {
    /// How many `press` moves.
    #[must_use]
    pub(crate) const fn moved(&self, press: EveryPress) -> usize {
        use crate::autopilot_timers::TimerChoice;
        match press {
            EveryPress::Mode(Autopilot::Cruise) => self.cruise,
            EveryPress::Mode(Autopilot::Land) => self.land,
            EveryPress::Timer(TimerChoice::In(_) | TimerChoice::At) => self.timer,
            EveryPress::Follow => self.follow,
            EveryPress::Mode(Autopilot::Off) | EveryPress::Timer(TimerChoice::Cancel) => 0,
        }
    }

    /// What is there now, beside the buttons: `6 sessions: 3 cruising, 1 landing, 1 landed, 1 off` (`3 cruising (2 timed)` where timers are armed; a session under Land by its chip's face, `landing`, `landed` or `quiet`, §12.4.1; a zero part is not said); where the harness steers none of them, why, in the rows' own short words (`3 sessions · harness off in Settings`), which fit where a row's detail fits.
    #[must_use]
    pub(crate) fn line(&self) -> String {
        let all = sessions_words(self.sessions);
        if self.steered == 0 {
            return format!("{all} \u{b7} {}", self.why);
        }
        let mut parts = Vec::new();
        if self.cruising > 0 {
            parts.push(if self.timed > 0 {
                format!("{} cruising ({} timed)", self.cruising, self.timed)
            } else {
                format!("{} cruising", self.cruising)
            });
        }
        parts.extend(under_land(self.landing, self.landed, self.quiet));
        if self.off > 0 {
            parts.push(format!("{} off", self.off));
        }
        format!("{all}: {}", parts.join(", "))
    }

    fn offs_stay(&self, off: usize) -> String {
        match off {
            0 => String::new(),
            1 => "; the 1 off stays off".to_string(),
            n => format!("; the {n} off stay off"),
        }
    }

    /// The timers a press cancels, for its words: Land's (every timed Cruise it lands) and Follow's (every timed own Cruise it clears).
    #[must_use]
    pub(crate) const fn cancels(&self, press: EveryPress) -> usize {
        match press {
            EveryPress::Mode(Autopilot::Land) => self.cruise_timed,
            EveryPress::Follow => self.follow_timed,
            EveryPress::Mode(_) | EveryPress::Timer(_) => 0,
        }
    }

    /// What a button's spoken name adds after its count: the timers it cancels (`, cancelling 2 timers`) and, for Follow under a default of Off, `, the default is Off`.
    #[must_use]
    pub(crate) fn spoken_tail(&self, press: EveryPress) -> String {
        let mut tail = String::new();
        if press == EveryPress::Follow && self.follow_to_off > 0 {
            tail.push_str(", the default is Off");
        }
        match self.cancels(press) {
            0 => {}
            1 => tail.push_str(", cancelling 1 timer"),
            n => tail.push_str(&format!(", cancelling {n} timers")),
        }
        tail
    }

    fn cancelling(&self, press: EveryPress) -> String {
        let timers = match self.cancels(press) {
            0 => return String::new(),
            1 => "1 timer".to_string(),
            n => format!("{n} timers"),
        };
        match press {
            EveryPress::Follow => format!(", {timers} cancelled"),
            EveryPress::Mode(_) | EveryPress::Timer(_) => format!(" and cancel {timers}"),
        }
    }

    /// What a press does, for its button's description (design §11.5.1): the count it moves, the timers it cancels and what stays — `Land 3 sessions and cancel 2 timers; the 1 off stays off`, `Cruise 4 sessions: 1 landing, 1 landed, 2 timed` (its sessions under Land told apart as the row tells them, §12.4.1) — where Follow hands sessions to a default of Off, that the harness lets go of them; or, where it moves nothing, that it writes nothing.
    #[must_use]
    pub(crate) fn description(&self, press: EveryPress) -> String {
        use crate::autopilot_timers::TimerChoice;
        let n = self.moved(press);
        let stays = self.offs_stay(self.off);
        let cancel = self.cancelling(press);
        match press {
            EveryPress::Mode(Autopilot::Land) if n == 0 => {
                "No session is cruising, so this changes nothing".to_string()
            }
            EveryPress::Mode(Autopilot::Land) => {
                format!("Land {}{cancel}{stays}", sessions_words(n))
            }
            EveryPress::Mode(Autopilot::Cruise) if n == 0 => {
                "No session is under Land or timed, so this changes nothing".to_string()
            }
            EveryPress::Mode(Autopilot::Cruise) => {
                let mut how =
                    under_land(self.cruise_landing, self.cruise_landed, self.cruise_quiet);
                if self.cruise_timed > 0 {
                    how.push(format!("{} timed", self.cruise_timed));
                }
                format!("Cruise {}: {}{stays}", sessions_words(n), how.join(", "))
            }
            EveryPress::Timer(TimerChoice::In(_) | TimerChoice::At) if n == 0 => {
                "No session is cruising, so this changes nothing".to_string()
            }
            EveryPress::Timer(TimerChoice::In(_) | TimerChoice::At) if n == 1 => {
                format!("Time 1 session to land{stays}")
            }
            EveryPress::Timer(TimerChoice::In(_) | TimerChoice::At) => {
                format!("Time {n} sessions to land together{stays}")
            }
            EveryPress::Follow => {
                let kept = match self.pinned_off {
                    0 => String::new(),
                    1 => "; the 1 turned off stays off".to_string(),
                    p => format!("; the {p} turned off stay off"),
                };
                let follow = if n == 1 { "follows" } else { "follow" };
                if n == 0 {
                    format!("Every session follows [harness] autopilot already{kept}")
                } else if self.follow_to_off > 0 {
                    let them = if n == 1 { "it" } else { "them" };
                    format!(
                        "{} {follow} [harness] autopilot, which is Off: the harness lets go \
                         of {them}{cancel}{kept}",
                        sessions_words(n)
                    )
                } else {
                    format!(
                        "{} {follow} [harness] autopilot again{cancel}{kept}",
                        sessions_words(n)
                    )
                }
            }
            EveryPress::Mode(Autopilot::Off) | EveryPress::Timer(TimerChoice::Cancel) => {
                String::new()
            }
        }
    }
}

fn session_title(resolved: Option<&str>, program: &str) -> String {
    resolved
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(program)
        .to_string()
}

fn names_program(title: &str, program: &str) -> bool {
    title
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .eq_ignore_ascii_case(program)
}

impl App {
    /// THE A/B BOOK'S WEEK (§10.5.3 B, C.4): today and the six days before, the open stretches counted to now, with the other processes' days of the last reading ([`HarnessStatsReport::ab_others`]) added, the local clock and day the earliest began ([`AbWeek::since`], one zone read: a publish's), and the publish's own clock, the instant the numbers stop at ([`AbWeek::until`], §11.6.3 item 3: one wall read, so a page republished past midnight says the new day's time).
    #[must_use]
    pub(crate) fn harness_ab(&self) -> AbWeek {
        let others = self
            .harness_screen
            .stats
            .report
            .as_ref()
            .map_or(&[][..], |report| report.ab_others.as_slice());
        let mut week = self
            .harness_screen
            .ab
            .week_at(crate::turn_ledger::now_ms(), others);
        week.since = week
            .first
            .and_then(|(_, unix_ms)| crate::harness_glass::Stamp::at(unix_ms.div_euclid(1000)));
        week.until =
            crate::harness_glass::Stamp::at(crate::autopilot_timers::wall_ms().div_euclid(1000));
        week
    }

    /// What session `local_id`'s glass says of why it may sit at its prompt ([`AbCause::of`]): whether its task's face says `landed` and whether the harness's pause stands.
    #[must_use]
    pub(crate) fn harness_ab_glass(&self, local_id: u64) -> (bool, bool) {
        use aterm_agent::harness::acts::TaskFace;
        self.harness_glass
            .get(&local_id)
            .map(crate::harness_glass::SessionGlass::chip)
            .map_or((false, false), |glass| {
                (glass.task == Some(TaskFace::Landed), glass.pause.is_some())
            })
    }

    /// THE WALL MOVED under the ledger clock (`Wake::WallMoved`, a window's focus): the A/B book's calendar is read again where it was read at all, so a sleep across a midnight starts the new day's stretches in the new day ([`AbBook::set_calendar`]).
    pub(crate) fn harness_ab_wall_moved(&mut self) {
        if self.harness_screen.ab.has_calendar() {
            self.harness_screen
                .ab
                .set_calendar(crate::harness_ab::AbCalendar::read(
                    crate::turn_ledger::now_ms(),
                    crate::autopilot_timers::wall_ms(),
                    crate::autopilot_timers::local_offset_at,
                ));
        }
    }

    /// THE EXIT SEAM'S WRITE (§10.5.3 C.2): this process's A/B book, its open stretches counted to now, written whole to its own file (`harness_ab_store::write_own`) once the event loop has returned — one small write, never on the frame path.
    pub(crate) fn write_harness_ab_at_exit(&self) {
        let Some(harness_state) = crate::harness_host::harness_state() else {
            return;
        };
        let Some((ring, today)) = self.harness_screen.ab.ring_at(crate::turn_ledger::now_ms())
        else {
            return;
        };
        if let Err(error) = self
            .harness_screen
            .ab_writes
            .snapshot((ring, today))
            .and_then(|snapshot| {
                snapshot.write_if_newer(
                    &harness_state,
                    &crate::harness_ab_store::own_name(),
                    unix_ms_now(),
                )
            })
        {
            aterm_log::warn!("harness A/B book: not written at exit: {error}");
        }
    }

    /// Session `local_id`'s A/B arm now ([`AbArm::of`] its effective Autopilot, this host's claim and anyone's).
    #[must_use]
    pub(crate) fn harness_ab_arm(&self, local_id: u64) -> AbArm {
        self.harness_ab_reading(local_id).0
    }

    /// [`Self::harness_ab_arm`], and whether the session's MODE is Land — its own word, else the table's default, whatever the posture or the claim (`AbBook::follow_mode`, §11.6.3 item 4) — from the same one read of its metadata.
    #[must_use]
    pub(crate) fn harness_ab_reading(&self, local_id: u64) -> (AbArm, bool) {
        let default = self.harness_policy.get().policy.autopilot;
        let Some(session) = self.pool.get(local_id) else {
            return (
                AbArm::of(
                    AutopilotView::of(self.harness_posture(), None),
                    false,
                    false,
                ),
                default == Autopilot::Land,
            );
        };
        let posture = self.session_harness_posture(local_id);
        let (word, ours) = {
            let meta = session.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let ours = meta
                .live_supervisor(crate::metrics::now_us())
                .map(|holder| holder == crate::autopilot::own_holder());
            (meta.autopilot_word(), ours)
        };
        (
            AbArm::of(
                AutopilotView::of(posture, word),
                ours == Some(true),
                ours.is_some(),
            ),
            word.unwrap_or(default) == Autopilot::Land,
        )
    }

    /// Session `local_id`'s published word moved and no pool borrow is live (an input stall's publish): [`HarnessScreen::note_phase`].
    pub(crate) fn note_harness_phase(&mut self, local_id: u64) {
        let arm = self.harness_ab_arm(local_id);
        let glass = self.harness_ab_glass(local_id);
        let Some(ctx) = self.pool.get(local_id).map(|s| s.ctx.clone()) else {
            return;
        };
        self.harness_screen.note_phase(local_id, &ctx, arm, glass);
    }

    /// Session `local_id`'s ARM or CAUSE may have moved (its Autopilot word written, its claim taken or given back, its task's face or the harness's pause told): the stretch so far goes to the arm and cause that were in force, and the session counts under its arm and cause now from here.
    pub(crate) fn note_harness_arm(&mut self, local_id: u64) {
        let (arm, lands) = self.harness_ab_reading(local_id);
        let glass = self.harness_ab_glass(local_id);
        let Some(session) = self.pool.get(local_id) else {
            return;
        };
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
        let cause = AbCause::of(arm, phase, glass.0, glass.1);
        self.harness_screen.ab_calendar();
        let now = crate::turn_ledger::now_ms();
        let book = &mut self.harness_screen.ab;
        book.follow_mode(local_id, lands, now);
        book.observe(local_id, phase, cause, arm, now);
    }

    /// Session `local_id`'s word was CARRIED onto it from before this process (`App::graft_restored_user_meta`), its mode read as Land (`lands_before`) or not just before: where the carried word moved the mode, the A/B book takes it as read with no move (`AbBook::carry_mode`), so a landing restored or handed over mid-work is counted with no span.
    pub(crate) fn harness_mode_carried(&mut self, local_id: u64, lands_before: bool) {
        let lands = self.harness_ab_reading(local_id).1;
        if lands != lands_before {
            self.harness_screen.ab.carry_mode(local_id, lands);
        }
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

    /// THE SNAPSHOT Settings ▸ Harness renders: the policy in force, every live supervisable agent session, the ledgers' last reading, the A/B book's week now and the effective table.
    #[must_use]
    pub(crate) fn harness_state(&self) -> HarnessState {
        let posture = self.harness_posture();
        let policy = self.harness_policy.get();
        let sessions = self.harness_sessions(posture);
        let decided_open = self.harness_decided_open(&sessions);
        HarnessState {
            observed: true,
            words: policy.words(),
            posture,
            sessions,
            decided_open,
            stats: self.harness_screen.stats.clone(),
            ab: self.harness_ab(),
            config: config_rows(&policy.policy),
            land_at_offer: policy.policy.land_at.map(|at| {
                crate::autopilot_timers::offer_glass(
                    at,
                    crate::autopilot_timers::wall_ms(),
                    &crate::autopilot_timers::local_offset_at,
                )
            }),
            ai: self.ai_state(),
            tabs: self.tab_control_state(posture),
            today: crate::harness_glass::local_day_at(
                crate::autopilot_timers::wall_ms().div_euclid(1000),
            ),
        }
    }

    /// Snapshot of all accessible computers, `aterm` instances, and open tabs for Settings ▸ Harness ▸ Tab Control (`/tabs`).
    #[must_use]
    pub(crate) fn tab_control_state(&self, posture: HarnessPosture) -> TabControlState {
        let handles = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        let holder = crate::harness_host::holder();
        let now_us = crate::metrics::now_us();
        let local_comp_name = local_computer_label();
        let own_instance = format!("this aterm (pid {})", std::process::id());
        let mut local_tabs = Vec::new();
        for handle in handles.iter().filter(|h| {
            h.state != crate::session_store::SessionState::Exited
                && self.pool.get(h.local_id).is_some()
        }) {
            let (raw_program, agent_prog, word, detail, subject, since_ms, task_face) = {
                let tl = handle
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let ag = tl.agent();
                (
                    ag.program.clone(),
                    crate::harness_host::agent_of(ag),
                    ag.word,
                    ag.detail.clone(),
                    ag.subject.clone(),
                    ag.since_ms(),
                    tl.task_face().map(|(f, _)| f.status()),
                )
            };
            let (person, auto_word, supervised) = {
                let meta = handle.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                (
                    meta.presentation_value("title"),
                    meta.autopilot_word(),
                    meta.live_supervisor(now_us) == Some(holder.as_str()),
                )
            };
            let admitted = crate::harness_host::admitted(&handle.ctx);
            let (program_label, is_ai_cli) = if let Some(p) = admitted.or(agent_prog) {
                (p.product().to_string(), true)
            } else if let Some(raw) = raw_program
                .as_deref()
                .filter(|s| !s.is_empty() && *s != "-")
            {
                if let Some(prod) = ai_product_for_name(raw) {
                    (prod.to_string(), true)
                } else {
                    (raw.to_string(), false)
                }
            } else {
                ("shell".to_string(), false)
            };
            let title = self
                .harness_session_name(handle.local_id, person, &handle.title)
                .unwrap_or_else(|| handle.display_title());
            let (cols, rows, cwd) = self
                .pool
                .get(handle.local_id)
                .and_then(|s| match s.term.try_lock() {
                    Ok(t) => {
                        use crate::cwd_native::ReportedCwd as _;
                        Some((
                            t.cols(),
                            t.rows(),
                            t.native_working_directory()
                                .map(std::borrow::Cow::into_owned),
                        ))
                    }
                    Err(std::sync::TryLockError::Poisoned(p)) => {
                        use crate::cwd_native::ReportedCwd as _;
                        let t = p.into_inner();
                        Some((
                            t.cols(),
                            t.rows(),
                            t.native_working_directory()
                                .map(std::borrow::Cow::into_owned),
                        ))
                    }
                    Err(std::sync::TryLockError::WouldBlock) => None,
                })
                .unwrap_or((0, 0, None));
            let base_place = place_words(&self.session_place(handle.sid.as_str()));
            let place = match (cols > 0 && rows > 0, cwd.as_deref()) {
                (true, Some(dir)) if !dir.is_empty() => {
                    format!(
                        "{base_place} ({cols}\u{d7}{rows} \u{b7} {})",
                        shorten_home_dir(dir)
                    )
                }
                (true, _) => format!("{base_place} ({cols}\u{d7}{rows})"),
                (false, Some(dir)) if !dir.is_empty() => {
                    format!("{base_place} ({})", shorten_home_dir(dir))
                }
                _ => base_place,
            };
            let autopilot = if is_ai_cli {
                let view = AutopilotView::of(posture.for_agent(admitted.or(agent_prog)), auto_word);
                if supervised {
                    format!("{} (supervised)", view.wire())
                } else {
                    view.wire().to_string()
                }
            } else {
                "-".to_string()
            };
            let ai_status = if is_ai_cli {
                format_ai_cli_live_status(
                    (word != "-").then_some(word),
                    detail.as_deref(),
                    subject.as_deref(),
                    (word != "-").then_some(since_ms),
                    task_face,
                )
            } else {
                "-".to_string()
            };
            let human_wire = handle.ctx.human_input.wire(now_us);
            let human = human_activity_words(human_wire.parse::<u64>().ok());
            let vitals = self
                .vitals
                .snapshot
                .as_deref()
                .and_then(|snap| snap.tabs.iter().find(|tv| tv.key == handle.local_id))
                .map_or_else(String::new, |tv| {
                    let cpu_pct = (tv.scope.cores.level * 100.0).round().max(0.0);
                    match tv.scope.footprint {
                        Some(mem) => format!(
                            "CPU {cpu_pct:.0}% ({:.2}c) \u{b7} RAM {}",
                            tv.scope.cores.level,
                            human_bytes_gui(mem)
                        ),
                        None => format!("CPU {cpu_pct:.0}% ({:.2}c)", tv.scope.cores.level),
                    }
                });
            local_tabs.push(TabSpreadsheetRow {
                key: format!("local/{}", handle.local_id),
                computer: local_comp_name.clone(),
                instance: own_instance.clone(),
                place,
                sid: handle.sid.as_str().to_string(),
                title,
                state: handle.state.as_str().to_string(),
                program: program_label,
                is_ai_cli,
                ai_status,
                autopilot,
                human,
                vitals,
                show_local_id: Some(handle.local_id),
            });
        }
        local_tabs.sort_by_key(|row| row.show_local_id.unwrap_or(u64::MAX));

        let (sibling_tabs, mut remote_computers, remote_tabs) =
            self.harness_screen.stats.report.as_deref().map_or_else(
                || (Vec::new(), Vec::new(), Vec::new()),
                |r| {
                    (
                        r.sibling_tabs.clone(),
                        r.remote_computers.clone(),
                        r.remote_tabs.clone(),
                    )
                },
            );
        let local_meta = remote_computers
            .iter()
            .position(|c| c.id == "__local_meta__")
            .map(|idx| remote_computers.remove(idx));
        let sibling_instances: std::collections::BTreeSet<&str> =
            sibling_tabs.iter().map(|t| t.instance.as_str()).collect();
        let local_instances = 1 + sibling_instances.len();
        let local_ai_clis = local_tabs
            .iter()
            .chain(sibling_tabs.iter())
            .filter(|t| t.is_ai_cli)
            .count();
        let local_active_ai = local_tabs
            .iter()
            .chain(sibling_tabs.iter())
            .filter(|t| {
                t.is_ai_cli
                    && (t.ai_status.starts_with("working") || t.ai_status.starts_with("waiting"))
            })
            .count();
        let live_local_vitals = self
            .vitals
            .snapshot
            .as_deref()
            .map(format_local_snapshot_vitals);
        let (self_display_name, self_address, self_os_version, self_status, self_vitals) =
            match local_meta {
                Some(m) => (
                    m.name,
                    m.address,
                    m.os_version,
                    m.status,
                    live_local_vitals
                        .filter(|v| !v.is_empty())
                        .unwrap_or(m.vitals),
                ),
                None => (
                    format!("{local_comp_name} (this Mac)"),
                    "local".to_string(),
                    std::env::consts::OS.to_string(),
                    "online".to_string(),
                    live_local_vitals.unwrap_or_default(),
                ),
            };
        let mut computers = vec![TabComputer {
            id: "local".to_string(),
            name: self_display_name,
            address: self_address,
            os_version: self_os_version,
            status: self_status,
            vitals: self_vitals,
            instances: local_instances,
            tabs: local_tabs.len() + sibling_tabs.len(),
            ai_clis: local_ai_clis,
            active_ai: local_active_ai,
        }];
        computers.extend(remote_computers);

        let mut tabs = local_tabs;
        tabs.extend(sibling_tabs);
        tabs.extend(remote_tabs);
        TabControlState {
            computers,
            tabs,
            fleet: FleetScope::of_this_process(),
        }
    }

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
            let program = crate::harness_host::admitted(&handle.ctx);
            let session_posture = posture.for_agent(program);
            let Some(program) = program.filter(|p| p.harness_table()).map(|p| p.product()) else {
                continue;
            };
            let (person, word, supervised, armed_at_ms, set_by) = {
                let meta = handle.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                (
                    meta.presentation_value("title"),
                    meta.autopilot_word(),
                    meta.live_supervisor(now_us) == Some(holder.as_str()),
                    meta.land_at
                        .filter(|_| meta.autopilot_word() == Some(Autopilot::Cruise)),
                    meta.autopilot_set_by,
                )
            };
            let set_by = word.and_then(|_| {
                crate::autopilot::SetByWords::of(set_by, true, |sid| self.autopilot_setter_tab(sid))
            });
            let resolved = self.harness_session_name(handle.local_id, person, &handle.title);
            let need = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .need()
                .harness_ask()
                .cloned();
            let hold = self.shown_hold(handle.local_id).filter(|_| supervised);
            let stopped = self.shown_stop(handle.local_id).filter(|_| supervised);
            sessions.push(HarnessSession {
                local_id: handle.local_id,
                sid: handle.sid.as_str().to_string(),
                title: session_title(resolved.as_deref(), program),
                place: self.session_place(handle.sid.as_str()),
                program,
                word,
                view: AutopilotView::of(session_posture, word),
                default: session_posture.default_mode(),
                supervised,
                glass: self.chip_glass(handle.local_id),
                armed_at_ms,
                set_by,
                need,
                hold,
                stopped,
            });
        }
        sessions.sort_by_key(|s| s.local_id);
        sessions
    }

    fn harness_decided_open(&self, sessions: &[HarnessSession]) -> Vec<OpenSession> {
        let Some(decided) = self
            .harness_screen
            .stats
            .report
            .as_deref()
            .and_then(|report| report.today.decided.as_ref())
        else {
            return Vec::new();
        };
        let wanted: std::collections::BTreeSet<&str> = decided
            .rows
            .iter()
            .map(|decision| decision.sid.as_str())
            .filter(|sid| !sid.is_empty() && !sessions.iter().any(|s| s.sid == *sid))
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        };
        let handles = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        handles
            .iter()
            .filter(|h| {
                h.state != crate::session_store::SessionState::Exited
                    && self.pool.get(h.local_id).is_some()
                    && wanted.contains(h.sid.as_str())
            })
            .map(|h| {
                let person = h
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .presentation_value("title");
                OpenSession {
                    sid: h.sid.as_str().to_string(),
                    local_id: h.local_id,
                    title: self
                        .harness_session_name(h.local_id, person, &h.title)
                        .unwrap_or_else(|| h.display_title()),
                }
            })
            .collect()
    }

    /// A SESSION'S NAME as Settings ▸ Harness and its Autopilot menu give it: the person's name for its tab (`person`, the `title` its metadata carries), else THE TAB'S NAME AS THE STRIP RESOLVES IT, from the terminal it shows: the store's copy of the title (`stored`) is written only as the front tab's window title is published, so it is stale for a background tab, a pane the keyboard is not in, with Settings in front, and on a headless aterm.
    pub(crate) fn harness_session_name(
        &self,
        local_id: u64,
        person: Option<String>,
        stored: &str,
    ) -> Option<String> {
        if let Some(person) = person.filter(|person| !person.trim().is_empty()) {
            return Some(person);
        }
        let session = self.pool.get(local_id)?;
        let rung = |t: &aterm_core::terminal::Terminal| {
            use crate::cwd_native::ReportedCwd as _;
            let cwd = t.native_working_directory();
            crate::app_tabs::resolved_terminal_title_rung(None, t.title(), cwd.as_deref())
        };
        match session.term.try_lock() {
            Ok(t) => rung(&t),
            Err(std::sync::TryLockError::Poisoned(p)) => rung(&p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                crate::app_tabs::resolved_terminal_title_rung(None, stored, None)
            }
        }
        .map(|title| crate::chrome_label::without_spinner_frame(&title).to_owned())
    }

    /// Whether some Settings view is on the Harness or Tab Control route now.
    pub(crate) fn harness_page_shown(&self) -> bool {
        self.view_store.iter().any(|(view, link)| {
            matches!(link, crate::tab_model::View::Native(_))
                && (self.harness_page_in(view) || self.tabs_page_in(view))
        })
    }

    /// Whether native view `view` is a Settings view on the Harness route — what the control surface's front-input fence asks of the front view (`App::front_control_surface`), since its session buttons are Owner-only ([`SESSION_ACTION_PREFIX`]).
    pub(crate) fn harness_page_in(&self, view: crate::tab_model::ViewId) -> bool {
        matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Harness
        )
    }

    /// Whether native view `view` is a Settings view on the Tab Control route.
    pub(crate) fn tabs_page_in(&self, view: crate::tab_model::ViewId) -> bool {
        matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Tabs
        )
    }

    /// Whether some Settings view is on the Tab Control / Fleet Monitor (`/tabs` / `/monitor`) route now.
    pub(crate) fn tabs_page_shown(&self) -> bool {
        self.view_store.iter().any(|(view, link)| {
            matches!(link, crate::tab_model::View::Native(_)) && self.tabs_page_in(view)
        })
    }

    /// Something the page shows moved (a host note, a claim, a session's end): republish while a view shows the page.
    pub(crate) fn harness_screen_moved(&mut self) {
        if self.harness_page_shown() {
            self.publish_native_harness_state();
        }
    }

    /// Perform a Settings ▸ Harness request (`AppEffect::Harness`): read the ledgers again, or set a session's Autopilot through the one writer ([`Self::set_session_autopilot`], which republishes).
    pub(crate) fn execute_native_harness(&mut self, request: crate::native_app::HarnessRequest) {
        match request {
            crate::native_app::HarnessRequest::Refresh => {
                self.start_harness_stats_refresh();
                self.publish_native_harness_state();
            }
            crate::native_app::HarnessRequest::Autopilot { session, word } => {
                self.press_page_mode(session, word);
            }
            crate::native_app::HarnessRequest::Every(press) => {
                self.press_autopilot_every(press);
            }
            crate::native_app::HarnessRequest::AiLook => self.ask_ai_look(),
            crate::native_app::HarnessRequest::AiFleetFetch => self.ask_ai_fetch(),
            crate::native_app::HarnessRequest::AiSignIn { id } => {
                if let Some(window) = self.frontmost_window {
                    self.ai_sign_in(window, id);
                }
            }
            crate::native_app::HarnessRequest::Show { session } => {
                if !self.raise_session_pane(session) {
                    self.publish_native_harness_state();
                }
            }
        }
    }

    /// A SESSIONS ROW'S PRESS of `mode` on session `local_id`: the page's one rule's word ([`page_word`]) written through the window's one writer ([`Self::set_session_autopilot`], which publishes the page where it moved), by the session's own posture — its agent's switch Off, or the master switch, steers nothing there, whatever the window's.
    pub(crate) fn press_page_mode(&mut self, local_id: u64, mode: Autopilot) -> bool {
        let Some(default) = self.session_harness_posture(local_id).default_mode() else {
            return false;
        };
        self.set_session_autopilot(
            local_id,
            page_word(mode, default),
            crate::autopilot::Surface::Settings,
        )
    }

    /// A PRESS ON SESSIONS' `Every session` ROW (design §11.5.3): the live sessions SESSIONS lists, read again at the press (a session opened or closed since the draw is counted as it is now), each moved where [`EveryPress::moves`] says — never one whose mode is Off, and none the harness cannot steer — and written as [`EveryPress::write`] says, a timer at ONE instant resolved here once ([`Self::resolve_timer_choice`]), so every armed timer shares it and the index folds one `autopilot_timer` deadline.
    pub(crate) fn press_autopilot_every(&mut self, press: EveryPress) -> usize {
        let sessions = self.harness_sessions(self.harness_posture());
        let at = match press {
            EveryPress::Timer(choice) => self.resolve_timer_choice(choice),
            EveryPress::Mode(_) | EveryPress::Follow => None,
        };
        let writes: Vec<(u64, Option<Autopilot>, Option<i64>)> = sessions
            .iter()
            .filter(|session| press.moves(session))
            .filter(|_| !matches!(press, EveryPress::Timer(_)) || at.is_some())
            .filter_map(|session| {
                let (word, land_at) = press.write(session.default?, at);
                Some((session.local_id, word, land_at))
            })
            .collect();
        self.write_autopilot_batch(
            &writes,
            aterm_slog::Actor::Human,
            Some(crate::autopilot::Surface::EverySession),
        )
    }

    /// AN ACT, TOLD WHILE THE PAGE MAY BE ON SCREEN (§9.2.1, "The page"; `crate::harness_glass::on_act_at`, at the act's receipt `now`, for EVERY act, whether or not the chip's trail took it): with the page on screen, WHAT THE HARNESS DID is marked stale and a re-read owed ([`HarnessScreen::note_act`]: [`crate::harness_glass::DWELL`] after the act, at most one per [`crate::harness_glass::REREAD`], on the glass's one deadline), and the page republished where the mark or the session's glass (`glass_moved`) moved.
    pub(crate) fn note_harness_act_for_page(&mut self, now: Instant, glass_moved: bool) {
        if !self.harness_page_shown() {
            return;
        }
        if self.harness_screen.note_act(now) || glass_moved {
            self.publish_native_harness_state();
        }
    }

    /// The re-read the page owes is due at `now`: started while the page is on screen — where a reading is already running it answers instead, and owes another if it lands not covering the acts ([`Self::finish_harness_stats`]) — and forgotten where the page has gone.
    pub(crate) fn reread_harness_stats(&mut self, now: Instant) {
        if !self.harness_page_shown() {
            self.harness_screen.forget_acts();
            return;
        }
        self.harness_screen.take_reread();
        self.start_harness_stats_refresh_at(now);
        self.publish_native_harness_state();
    }

    /// READ THE LEDGERS AGAIN, off the event loop: one named background thread folds today and the last 7 days ([`collect_harness_stats`]), writes this process's A/B book and reads the others' ([`read_harness_stats`]), and posts `Wake::HarnessStatsCollected`.
    pub(crate) fn start_harness_stats_refresh(&mut self) {
        self.start_harness_stats_refresh_at(Instant::now());
    }

    /// [`Self::start_harness_stats_refresh`], begun at `now`: the reading meets the freshness threshold for acts told a dwell or more before it ([`HarnessScreen::began_reading`]).
    pub(crate) fn start_harness_stats_refresh_at(&mut self, now: Instant) {
        #[cfg(test)]
        {
            self.harness_screen.readings_asked =
                self.harness_screen.readings_asked.saturating_add(1);
        }
        if self.harness_screen.stats.collecting {
            let ask = self.fleet_ask();
            if ask.fleet
                && !self.harness_screen.reading.fleet
                && self.harness_screen.fleet_slot.raise(ask)
            {
                self.harness_screen.reading = ask;
            }
            return;
        }
        let state_dir = aterm_types::dirs::state_dir();
        let harness_state = crate::harness_host::harness_state();
        let ab = self
            .harness_screen
            .ab
            .ring_at(crate::turn_ledger::now_ms())
            .and_then(|ring| match self.harness_screen.ab_writes.snapshot(ring) {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    aterm_log::warn!("harness A/B book: snapshot not captured: {error}");
                    None
                }
            });
        let own = crate::harness_ab_store::own_name();
        let fleet = self.fleet_ask();
        #[cfg(test)]
        {
            self.harness_screen.last_fleet_ask = Some(fleet);
        }
        self.harness_screen.reading = fleet;
        let slot = std::sync::Arc::new(FleetSlot::new(fleet));
        self.harness_screen.fleet_slot = std::sync::Arc::clone(&slot);
        let Some(proxy) = self.proxy.clone() else {
            if !cfg!(test) && aterm_uds::test_binary::scratch_root().is_none() {
                self.harness_screen.sequence = self.harness_screen.sequence.wrapping_add(1);
                let sequence = self.harness_screen.sequence;
                self.harness_screen.began_reading(now);
                let result = read_harness_stats(
                    state_dir.as_deref(),
                    harness_state.as_deref(),
                    ab.as_ref(),
                    &own,
                    unix_ms_now(),
                    crate::presence::local_offset_at,
                    &slot,
                )
                .map(Box::new);
                self.finish_harness_stats(sequence, result);
            }
            return;
        };
        self.harness_screen.sequence = self.harness_screen.sequence.wrapping_add(1);
        let sequence = self.harness_screen.sequence;
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-harness-stats",
            crate::qos::Role::Background,
            move || {
                let result = std::panic::catch_unwind(|| {
                    read_harness_stats(
                        state_dir.as_deref(),
                        harness_state.as_deref(),
                        ab.as_ref(),
                        &own,
                        unix_ms_now(),
                        crate::presence::local_offset_at,
                        &slot,
                    )
                })
                .unwrap_or_else(|_| Err("the ledgers' reader failed".to_string()))
                .map(Box::new);
                let _ = proxy.send_event(crate::Wake::HarnessStatsCollected { sequence, result });
            },
        );
        match spawn {
            Ok(_) => self.harness_screen.began_reading(now),
            Err(error) => {
                self.harness_screen.stats.stale = false;
                self.harness_screen.stats.unavailable =
                    Some(format!("the reader could not start: {error}"));
            }
        }
    }

    /// The ledgers' reading `sequence` landed: kept when it is the running one's, and published.
    pub(crate) fn finish_harness_stats(
        &mut self,
        sequence: u64,
        result: Result<Box<HarnessStatsReport>, String>,
    ) {
        if sequence != self.harness_screen.sequence || !self.harness_screen.stats.collecting {
            return;
        }
        let shown = self.harness_page_shown();
        let fleet_owed =
            !self.harness_screen.reading.fleet && self.tabs_page_shown() && result.is_ok();
        let screen = &mut self.harness_screen;
        let covered = screen.covers_the_acts();
        screen.stats.collecting = false;
        match result {
            Ok(mut report) => {
                // A reading that asked nothing of the fleet keeps the fleet
                // the last one read: Tab Control shows its last sweep, never
                // an empty fleet, until its next.
                if !screen.reading.fleet
                    && let Some(last) = screen.stats.report.as_deref()
                {
                    report.sibling_tabs.clone_from(&last.sibling_tabs);
                    report.remote_computers.clone_from(&last.remote_computers);
                    report.remote_tabs.clone_from(&last.remote_tabs);
                }
                screen.stats.report = Some(std::sync::Arc::from(report));
                screen.stats.unavailable = None;
                if covered {
                    screen.stats.stale = false;
                } else if let Some(last) = screen.last_act.filter(|_| shown) {
                    screen.stats.stale = true;
                    screen.owe_reread(last);
                }
            }
            Err(why) => {
                screen.stats.unavailable = Some(why);
                screen.stats.stale = false;
            }
        }
        if !shown {
            screen.forget_acts();
        }
        if fleet_owed {
            self.start_harness_stats_refresh();
        }
        self.publish_native_harness_state();
    }

    /// AN UPDATE'S SUCCESSOR READS AT ITS COMMIT: before the Commit (`Wake::ActivateCommittedHandoff`) a reading's wake is dropped (`drops_wake_before_commit`), so a restored Settings ▸ Harness (`App::restore_native_leaf`) starts its reading here; without it the page would draw this process's book alone — `Ends: none yet` over the predecessor's landing — until a Refresh.
    #[cfg(unix)]
    pub(crate) fn settle_harness_reading_at_commit(&mut self) {
        self.harness_screen.stats.collecting = false;
        if self.harness_page_shown() {
            self.start_harness_stats_refresh();
            self.publish_native_harness_state();
        }
    }

    /// What a reading begun now asks of the fleet ([`FleetAsk`]): the fleet only while a Tab Control / Fleet Monitor page is shown, and this Mac's vitals on the reading's thread only where `VitalsHost` has no snapshot for that page yet.
    pub(crate) fn fleet_ask(&self) -> FleetAsk {
        let fleet = self.tabs_page_shown();
        FleetAsk {
            fleet,
            local_vitals: fleet && self.vitals.snapshot.is_none(),
        }
    }
}

/// Format live AI CLI status for a tab row (`"working: thinking (14s)"`, `"waiting on permission: Bash (5s)"`, `"idle at prompt · landed"`).
#[must_use]
pub(crate) fn format_ai_cli_live_status(
    word: Option<&str>,
    detail: Option<&str>,
    subject: Option<&str>,
    since_ms: Option<u64>,
    task: Option<&str>,
) -> String {
    fn clean(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty() && *s != "-")
    }
    let word = clean(word);
    let detail = clean(detail);
    let subject = clean(subject);
    let task = clean(task);

    let mut base = match word {
        Some("busy" | "working") => match (detail, task) {
            (Some(d), _) => format!("working: {d}"),
            (None, Some(t)) => format!("working \u{b7} task {t}"),
            (None, None) => "working".to_string(),
        },
        Some("prompt" | "idle") => {
            let prompt = match detail {
                Some(d) => format!("at prompt: {d}"),
                None => "idle at prompt".to_string(),
            };
            match task {
                Some(t) => format!("{prompt} \u{b7} {t}"),
                None => prompt,
            }
        }
        Some(w) if w == "permission" || w == "box:permission" || w == "waiting" => {
            match subject.or(detail) {
                Some(what) => format!("waiting on permission: {what}"),
                None => "waiting on permission".to_string(),
            }
        }
        Some(w) if w == "question" || w == "box:question" => match subject.or(detail) {
            Some(what) => format!("waiting on question: {what}"),
            None => "waiting on question".to_string(),
        },
        Some(w) if w == "limit" || w == "wall:rate-limit" => match detail {
            Some(d) => format!("rate limit: {d}"),
            None => "paused at rate limit".to_string(),
        },
        Some("wall:unresponsive") => {
            if detail == Some("stopped") {
                "stopped (Ctrl-Z)".to_string()
            } else {
                "unresponsive".to_string()
            }
        }
        Some(w) => match detail {
            Some(d) => format!("{w}: {d}"),
            None => w.to_string(),
        },
        None => "running".to_string(),
    };
    if let Some(ms) = since_ms.filter(|ms| *ms >= 1_000) {
        let dur = if ms < 60_000 {
            format!("{}s", ms / 1_000)
        } else {
            duration_words(ms)
        };
        base.push_str(&format!(" ({dur})"));
    }
    base
}

/// Format elapsed human input milliseconds (`"12s ago"`, `"5m ago"`, `"-"`).
#[must_use]
pub(crate) fn human_activity_words(human_ms: Option<u64>) -> String {
    match human_ms {
        None => "-".to_string(),
        Some(ms) if ms < 60_000 => format!("{}s ago", (ms / 1_000).max(1)),
        Some(ms) => format!("{} ago", duration_words(ms)),
    }
}

fn ai_product_for_name(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_ascii_lowercase();
    for prog in aterm_phase::Program::ALL {
        if prog.harness_table() {
            let prod = prog.product();
            if prod.eq_ignore_ascii_case(&lower) {
                return Some(prod);
            }
        }
    }
    match lower.as_str() {
        "claude" | "claude-code" => Some("Claude Code"),
        "codex" | "codex-cli" => Some("Codex"),
        "codex-alab" => Some("codex-alab"),
        "agy" | "antigravity" | "antigravity-cli" => Some("Antigravity CLI"),
        "grok" | "grok-build" | "grok build" => Some("Grok"),
        "kimi" | "kimi-code" | "kimi-cli" | "kimi code" => Some("Kimi Code"),
        "muse" | "muse-code" => Some("Muse Code"),
        _ => None,
    }
}

fn shorten_home_dir(path: &str) -> String {
    let trimmed = path.trim();
    if let Some(home) = aterm_types::dirs::home_dir()
        && let Some(h) = home.to_str().filter(|h| !h.is_empty())
    {
        if trimmed == h {
            return "~".to_string();
        }
        if let Some(rest) = trimmed.strip_prefix(h)
            && rest.starts_with('/')
        {
            return format!("~{rest}");
        }
    }
    trimmed.to_string()
}

fn host_tag_key_gui(raw: &str) -> String {
    let base = raw.trim().trim_end_matches('.');
    let clean = base
        .strip_suffix(".local")
        .or_else(|| base.strip_suffix(".localdomain"))
        .unwrap_or(base);
    let tokens: Vec<&str> = clean
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|t| !t.is_empty())
        .collect();
    if let Some(&first) = tokens.first() {
        let lower = first.to_ascii_lowercase();
        if lower == "spark"
            && let Some(&second) = tokens.get(1)
        {
            return format!("spark-{}", second.to_ascii_lowercase());
        }
        if lower.starts_with('m')
            && lower.len() > 1
            && lower[1..].chars().all(|c| c.is_ascii_digit())
        {
            return lower;
        }
    }
    clean.to_ascii_lowercase()
}

/// Normalize any discovered or local host name into a consistent canonical hyphenated identifier (`m1-MacBook-Air-M2-24GB-1TB`, `m14-MacBook-Air-M5-32GB-4TB-prime`, `spark-49f7`), honoring any custom override in `[names]` of `machines.toml`.
pub(crate) fn canonical_host_name_gui(
    raw: &str,
    custom_names: &std::collections::BTreeMap<String, String>,
) -> String {
    let base = raw.trim().trim_end_matches('.');
    let trimmed = base
        .strip_suffix(".local")
        .or_else(|| base.strip_suffix(".localdomain"))
        .unwrap_or(base)
        .trim();
    if trimmed.is_empty() {
        return "this-Mac".to_string();
    }
    let tag = host_tag_key_gui(trimmed);
    let slug: String = trimmed.split_whitespace().collect::<Vec<_>>().join("-");
    for candidate in [trimmed, slug.as_str(), tag.as_str()] {
        if let Some((_, v)) = custom_names
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(candidate))
        {
            return v.clone();
        }
    }
    let mut raw_tokens: Vec<&str> = trimmed
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|t| !t.is_empty())
        .collect();
    if raw_tokens
        .last()
        .is_some_and(|last| last.eq_ignore_ascii_case("ssh"))
        && raw_tokens.len() > 1
    {
        raw_tokens.pop();
    }
    if raw_tokens.len() >= 2 && raw_tokens[0].eq_ignore_ascii_case("spark") {
        return format!("spark-{}", raw_tokens[1].to_ascii_lowercase());
    }
    let mut norm: Vec<String> = Vec::with_capacity(raw_tokens.len() + 1);
    for (idx, &tok) in raw_tokens.iter().enumerate() {
        let low = tok.to_ascii_lowercase();
        let mapped = if idx == 0
            && low.starts_with('m')
            && low.len() > 1
            && low[1..].chars().all(|c| c.is_ascii_digit())
        {
            low
        } else if idx > 0
            && low.starts_with('m')
            && low.len() == 2
            && low.as_bytes()[1].is_ascii_digit()
        {
            tok.to_ascii_uppercase()
        } else {
            match low.as_str() {
                "macbook" => "MacBook".to_string(),
                "air" => "Air".to_string(),
                "pro" => "Pro".to_string(),
                "max" => "Max".to_string(),
                "ultra" => "Ultra".to_string(),
                "mini" => "mini".to_string(),
                "imac" => "iMac".to_string(),
                "studio" => "Studio".to_string(),
                "intel" => "Intel".to_string(),
                "core" => "Core".to_string(),
                "i3" | "i5" | "i7" | "i9" | "prime" => low,
                _ if low.ends_with("gb")
                    && low[..low.len() - 2]
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.') =>
                {
                    format!("{}GB", &tok[..tok.len() - 2])
                }
                _ if low.ends_with("tb")
                    && low[..low.len() - 2]
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.') =>
                {
                    format!("{}TB", &tok[..tok.len() - 2])
                }
                _ if low.ends_with("ghz")
                    && low[..low.len() - 3]
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.') =>
                {
                    format!("{}GHz", &tok[..tok.len() - 3])
                }
                _ => tok.to_string(),
            }
        };
        norm.push(mapped);
    }
    if norm.len() >= 2
        && norm[0].starts_with('m')
        && norm[0][1..].chars().all(|c| c.is_ascii_digit())
    {
        if norm[1] == "Air" {
            norm.insert(1, "MacBook".to_string());
        } else if norm[1] == "MacBook"
            && norm.get(2).is_some_and(|t| {
                t != "Pro"
                    && t != "Air"
                    && (t.starts_with('M') || t.chars().next().is_some_and(|c| c.is_ascii_digit()))
            })
        {
            norm.insert(2, "Pro".to_string());
        }
    }
    norm.join("-")
}

fn load_custom_host_names() -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(path) =
        aterm_types::dirs::aterm_config_dir().map(|cfg| cfg.join("machines/machines.toml"))
        && let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(val) = aterm_toml::from_str::<aterm_toml::Value>(&content)
        && let Some(names) = val.get("names").and_then(|v| v.as_table())
    {
        for (k, v) in names {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

fn raw_local_host_name_gui() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        #[cfg(target_os = "macos")]
        if !cfg!(test)
            && aterm_uds::test_binary::scratch_root().is_none()
            && let Some(sc) =
                run_cmd_stdout_bounded("/usr/sbin/scutil", &["--get", "LocalHostName"], 80).or_else(
                    || run_cmd_stdout_bounded("/usr/sbin/scutil", &["--get", "ComputerName"], 80),
                )
        {
            let trimmed = sc.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        std::env::var("HOSTNAME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let h = aterm_uds::process::host_name()?;
                let base = h.trim().trim_end_matches('.');
                let short = base
                    .strip_suffix(".local")
                    .or_else(|| base.strip_suffix(".localdomain"))
                    .unwrap_or(base);
                (!short.is_empty()).then(|| short.to_string())
            })
            .unwrap_or_else(|| "this Mac".to_string())
    })
    .clone()
}

fn local_computer_label() -> String {
    let raw = raw_local_host_name_gui();
    if cfg!(test) || aterm_uds::test_binary::scratch_root().is_some() {
        raw
    } else {
        canonical_host_name_gui(&raw, &load_custom_host_names())
    }
}

fn collect_fleet_and_sibling_tabs(
    ask: FleetAsk,
) -> (
    Vec<TabSpreadsheetRow>,
    Vec<TabComputer>,
    Vec<TabSpreadsheetRow>,
) {
    if !ask.fleet {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    #[cfg(test)]
    FLEET_SWEEPS.with(|sweeps| sweeps.set(sweeps.get() + 1));
    if FleetScope::of_this_process() != FleetScope::Account {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let sibling_tabs = collect_sibling_aterm_tabs();
    let (remote_computers, remote_tabs) = collect_remote_machines_tabs(ask.local_vitals);
    (sibling_tabs, remote_computers, remote_tabs)
}

#[cfg(test)]
thread_local! {
    /// The counting seam on [`collect_fleet_and_sibling_tabs`]: the fleet sweeps this thread's readings began.
    pub(crate) static FLEET_SWEEPS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(unix)]
fn collect_sibling_aterm_tabs() -> Vec<TabSpreadsheetRow> {
    sibling_aterm_tabs_in(sibling_socket_dirs())
}

#[cfg(unix)]
fn sibling_socket_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = aterm_uds::control_socket_dir() {
        dirs.push(dir);
    }
    // An aterm started with `$XDG_RUNTIME_DIR` set binds elsewhere than one started without it;
    // both defaults under this account's home are read (only an account's own aterm sweeps).
    if let Some(home) = aterm_types::dirs::home_dir() {
        for rel in [
            ".local/state/aterm/run",
            "Library/Application Support/aterm",
        ] {
            let candidate = home.join(rel);
            if !dirs.contains(&candidate) {
                dirs.push(candidate);
            }
        }
    }
    dirs
}

#[cfg(unix)]
fn sibling_aterm_tabs_in(dirs: Vec<std::path::PathBuf>) -> Vec<TabSpreadsheetRow> {
    use std::io::{BufRead, BufReader, Write};
    let own_pid = std::process::id();
    let comp_name = local_computer_label();
    let mut rows = Vec::new();
    let mut pids: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(pid_str) = name
                .strip_prefix("aterm-")
                .and_then(|s| s.strip_suffix(".sock"))
            else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };
            if pid != own_pid && !pids.iter().any(|(p, _)| *p == pid) {
                pids.push((pid, path));
            }
        }
    }
    pids.sort_by_key(|(pid, _)| *pid);
    for (pid, sock_path) in pids {
        let Some(sock) = sock_path.to_str() else {
            continue;
        };
        let Ok((mut stream, Ok(token))) = aterm_ctl::connect_then_read_token(sock) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(150)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(150)));
        let req = format!("AUTH {token}\nsessions\n");
        if stream.write_all(req.as_bytes()).is_err() {
            continue;
        }
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let reader = BufReader::new(stream);
        let mut pid_rows: Vec<(u64, TabSpreadsheetRow)> = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            if let Some((local_id, row)) = parse_sibling_roster_line(&comp_name, pid, &line) {
                pid_rows.push((local_id, row));
            }
        }
        if !pid_rows.is_empty()
            && let Ok((mut stream2, _)) = aterm_ctl::connect_then_read_token(sock)
        {
            let _ = stream2.set_read_timeout(Some(std::time::Duration::from_millis(80)));
            let _ = stream2.set_write_timeout(Some(std::time::Duration::from_millis(80)));
            let mut batch = format!("AUTH {token}\n");
            for (local_id, _) in &pid_rows {
                batch.push_str(&format!("@{local_id} dims\n@{local_id} cwd\n"));
            }
            if stream2.write_all(batch.as_bytes()).is_ok() {
                let _ = stream2.flush();
                let _ = stream2.shutdown(std::net::Shutdown::Write);
                let mut r2 = BufReader::new(stream2);
                for (_, row) in &mut pid_rows {
                    let mut dims_line = String::new();
                    let mut dims_opt: Option<(u32, u32)> = None;
                    if r2.read_line(&mut dims_line).is_ok()
                        && let Some(rest) = dims_line.trim().strip_prefix("OK ")
                    {
                        let mut parts = rest.split_whitespace();
                        if let (Some(r), Some(c)) = (
                            parts.next().and_then(|x| x.parse::<u32>().ok()),
                            parts.next().and_then(|x| x.parse::<u32>().ok()),
                        ) && c > 0
                            && r > 0
                        {
                            dims_opt = Some((c, r));
                        }
                    }
                    let mut cwd_line = String::new();
                    let mut cwd_opt: Option<String> = None;
                    if r2.read_line(&mut cwd_line).is_ok()
                        && let Some(rest) = cwd_line.trim().strip_prefix("OK ")
                    {
                        let decoded = aterm_control::wire::pct_decode(rest.trim());
                        if !decoded.is_empty() && decoded != "-" {
                            cwd_opt = Some(shorten_home_dir(&decoded));
                        }
                    }
                    let facts = dims_opt
                        .map(|(c, r)| format!("{c}\u{d7}{r}"))
                        .into_iter()
                        .chain(cwd_opt);
                    for fact in facts {
                        row.place.push_str(crate::native_ui::FACT_SEPARATOR);
                        row.place.push_str(&fact);
                    }
                }
            }
        }
        enrich_sibling_tab_vitals(pid, &mut pid_rows);
        rows.extend(pid_rows.into_iter().map(|(_, r)| r));
    }
    rows
}

#[cfg(unix)]
fn enrich_sibling_tab_vitals(aterm_pid: u32, pid_rows: &mut [(u64, TabSpreadsheetRow)]) {
    if pid_rows.is_empty() {
        return;
    }
    let Some(sweep) = aterm_sysprobe::processes() else {
        return;
    };
    let mut children_by_ppid: std::collections::BTreeMap<
        u32,
        Vec<&aterm_sysprobe::ProcessReading>,
    > = std::collections::BTreeMap::new();
    for p in &sweep.procs {
        children_by_ppid.entry(p.ppid).or_default().push(p);
    }
    let mut direct_shells: Vec<u32> = children_by_ppid
        .get(&aterm_pid)
        .map(|list| list.iter().map(|p| p.pid).collect())
        .unwrap_or_default();
    direct_shells.sort_unstable();
    let subtree_mem_and_cpu = |root_pid: u32| -> (u64, u64) {
        let mut stack = vec![root_pid];
        let mut seen = std::collections::BTreeSet::new();
        let mut mem = 0u64;
        let mut cpu_ns = 0u64;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            if let Some(p) = sweep.procs.iter().find(|p| p.pid == cur) {
                mem = mem.saturating_add(p.footprint.unwrap_or(0));
                cpu_ns = cpu_ns.saturating_add(p.cpu_ns.unwrap_or(0));
            }
            if let Some(kids) = children_by_ppid.get(&cur) {
                for k in kids {
                    stack.push(k.pid);
                }
            }
        }
        (mem, cpu_ns)
    };
    for (idx, (_, row)) in pid_rows.iter_mut().enumerate() {
        let target_pid = direct_shells.get(idx).copied().unwrap_or(aterm_pid);
        let (mem, _cpu_ns) = subtree_mem_and_cpu(target_pid);
        if mem > 0 {
            row.vitals = format!("RAM {}", human_bytes_gui(mem));
        }
    }
}

#[cfg(not(unix))]
fn collect_sibling_aterm_tabs() -> Vec<TabSpreadsheetRow> {
    Vec::new()
}

#[cfg(unix)]
fn parse_sibling_roster_line(
    comp_name: &str,
    pid: u32,
    line: &str,
) -> Option<(u64, TabSpreadsheetRow)> {
    use aterm_control::wire::pct_decode;
    let mut tokens = line.split_whitespace();
    let local_id: u64 = tokens.next()?.parse().ok()?;
    let sid = tokens.next()?.to_string();
    let _parent = tokens.next()?;
    let state = tokens.next()?.to_string();
    if state != "Running" && state != "Idle" && state != "alive" {
        return None;
    }
    let raw_title = pct_decode(tokens.next()?);
    let mut window = "-".to_string();
    let mut active = false;
    let mut focused = false;
    let mut program: Option<String> = None;
    let mut agent: Option<String> = None;
    let mut agent_detail: Option<String> = None;
    let mut agent_since_ms: Option<u64> = None;
    let mut human_ms: Option<u64> = None;
    let mut autopilot: Option<String> = None;
    let mut role: Option<String> = None;
    let mut supervisor: Option<String> = None;
    let mut user_title: Option<String> = None;
    let mut task: Option<String> = None;
    for tok in tokens {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        match k {
            "window" => window = v.to_string(),
            "active" => active = v == "1" || v == "true",
            "wfocus" => focused = v == "1" || v == "true",
            "program" if v != "-" => program = Some(pct_decode(v)),
            "agent" if v != "-" => agent = Some(pct_decode(v)),
            "agent_detail" if v != "-" => agent_detail = Some(pct_decode(v)),
            "agent_since_ms" if v != "-" => agent_since_ms = v.parse().ok(),
            "human_ms" if v != "-" => human_ms = v.parse().ok(),
            "autopilot" if v != "-" => autopilot = Some(pct_decode(v)),
            "role" if v != "-" => role = Some(pct_decode(v)),
            "supervisor" if v != "-" => supervisor = Some(pct_decode(v)),
            "user_title" if v != "-" => user_title = Some(pct_decode(v)),
            "task" if v != "-" => task = Some(pct_decode(v)),
            _ => {}
        }
    }
    let title = user_title
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(raw_title);
    let raw_prog = program.unwrap_or_else(|| "shell".to_string());
    let (prog_label, is_ai_cli) = if let Some(prod) = ai_product_for_name(&raw_prog) {
        if prod.eq_ignore_ascii_case(&raw_prog) {
            (prod.to_string(), true)
        } else {
            (format!("{prod} ({raw_prog})"), true)
        }
    } else if agent.is_some() {
        (raw_prog, true)
    } else {
        (raw_prog, false)
    };
    let ai_status = if is_ai_cli {
        format_ai_cli_live_status(
            agent.as_deref(),
            agent_detail.as_deref(),
            None,
            agent_since_ms,
            task.as_deref(),
        )
    } else {
        "-".to_string()
    };
    let mut auto_full = autopilot.unwrap_or_else(|| "-".to_string());
    if let Some(sup) = supervisor.as_deref().filter(|s| !s.is_empty()) {
        auto_full.push_str(&format!(" (supervised by {sup})"));
    }
    if let Some(r) = role.as_deref().filter(|r| !r.is_empty()) {
        auto_full.push_str(&format!(" \u{b7} role {r}"));
    }
    let marks = format!(
        "{}{}",
        if active { "*" } else { "" },
        if focused { "!" } else { "" }
    );
    let place = if window != "-" {
        format!("Tab #{local_id}{marks} (Win {window})")
    } else {
        format!("Tab #{local_id}{marks}")
    };
    Some((
        local_id,
        TabSpreadsheetRow {
            key: format!("sibling/{pid}/{local_id}"),
            computer: comp_name.to_string(),
            instance: format!("aterm-{pid}"),
            place,
            sid,
            title,
            state,
            program: prog_label,
            is_ai_cli,
            ai_status,
            autopilot: auto_full,
            human: human_activity_words(human_ms),
            vitals: String::new(),
            show_local_id: None,
        },
    ))
}

fn collect_remote_machines_tabs(local_vitals: bool) -> (Vec<TabComputer>, Vec<TabSpreadsheetRow>) {
    let custom_names = load_custom_host_names();
    let mut configured: Vec<(String, String, String)> = Vec::new();
    let mut state_dir_opt = None;
    if let Some(config_dir) = aterm_types::dirs::aterm_config_dir() {
        let machines_dir = config_dir.join("machines");
        let toml_path = machines_dir.join("machines.toml");
        state_dir_opt = Some(machines_dir.join("state"));
        if let Ok(content) = std::fs::read_to_string(&toml_path)
            && let Ok(val) = aterm_toml::from_str::<aterm_toml::Value>(&content)
            && let Some(hosts) = val.get("host").and_then(|v| v.as_array())
        {
            for h in hosts {
                let uuid = h
                    .get("uuid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let raw_name = h
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = canonical_host_name_gui(&raw_name, &custom_names);
                let address = h
                    .get("address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !uuid.is_empty() && !name.is_empty() {
                    configured.push((uuid, name, address));
                }
            }
        }
    }
    let mut computers = Vec::new();
    let mut tabs = Vec::new();
    for (uuid, name, address) in configured {
        let state_json = state_dir_opt
            .as_ref()
            .and_then(|sd| std::fs::read_to_string(sd.join(format!("{uuid}.json"))).ok())
            .and_then(|s| aterm_json::from_str::<aterm_json::Value>(&s).ok());
        let last_seen = state_json
            .as_ref()
            .and_then(|v| v.get("last_seen"))
            .and_then(|v| v.as_u64());
        let last_error = state_json
            .as_ref()
            .and_then(|v| v.get("last_error"))
            .and_then(|v| v.as_str());
        let hello_json = state_json
            .as_ref()
            .and_then(|v| v.get("hello"))
            .and_then(|v| v.as_str())
            .and_then(|s| aterm_json::from_str::<aterm_json::Value>(s).ok());
        let os_version = hello_json
            .as_ref()
            .and_then(|h| h.get("os_version"))
            .and_then(|v| v.as_str())
            .map(|s| format!("macOS {s}"))
            .unwrap_or_else(|| "macOS".to_string());
        let status = match (last_seen, last_error) {
            (Some(_), None) => "online (ssh)".to_string(),
            (Some(_), Some(_)) => "standby".to_string(),
            (None, Some(_)) => "offline".to_string(),
            (None, None) => "configured".to_string(),
        };
        let mut instance_pids = std::collections::BTreeSet::new();
        let mut host_tabs = 0usize;
        let mut host_ai = 0usize;
        let mut host_active_ai = 0usize;
        if let Some(sessions) = hello_json
            .as_ref()
            .and_then(|h| h.get("sessions"))
            .and_then(|v| v.as_array())
        {
            for s in sessions {
                let inst_pid = s.get("instance_pid").and_then(|v| v.as_u64()).unwrap_or(0);
                if inst_pid > 0 {
                    instance_pids.insert(inst_pid);
                }
                let local_id = s.get("local_id").and_then(|v| v.as_u64()).unwrap_or(0);
                let sid = s
                    .get("sid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_string();
                let state = s
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Running")
                    .to_string();
                let title = s
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_string();
                let window = s.get("window").and_then(|v| v.as_str()).unwrap_or("-");
                let active = s.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                let raw_prog = s.get("program").and_then(|v| v.as_str()).unwrap_or("shell");
                let agent = s.get("agent").and_then(|v| v.as_str());
                let agent_detail = s.get("agent_detail").and_then(|v| v.as_str());
                let agent_since_ms = s.get("agent_since_ms").and_then(|v| v.as_u64());
                let human_ms = s.get("human_ms").and_then(|v| v.as_u64());
                let autopilot = s
                    .get("autopilot")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_string();
                let task = s.get("task").and_then(|v| v.as_str());
                let cols = s.get("cols").and_then(|v| v.as_u64());
                let rows_n = s.get("rows").and_then(|v| v.as_u64());
                let cwd = s.get("cwd").and_then(|v| v.as_str());
                let (prog_label, is_ai_cli) = if let Some(prod) = ai_product_for_name(raw_prog) {
                    (format!("{prod} ({raw_prog})"), true)
                } else if agent.is_some() {
                    (raw_prog.to_string(), true)
                } else {
                    (raw_prog.to_string(), false)
                };
                let ai_status = if is_ai_cli {
                    format_ai_cli_live_status(agent, agent_detail, None, agent_since_ms, task)
                } else {
                    "-".to_string()
                };
                if is_ai_cli {
                    host_ai += 1;
                    if ai_status.starts_with("working") || ai_status.starts_with("waiting") {
                        host_active_ai += 1;
                    }
                }
                host_tabs += 1;
                let mut place = if window != "-" {
                    format!(
                        "Tab #{local_id}{} (Win {window})",
                        if active { "*" } else { "" }
                    )
                } else {
                    format!("Tab #{local_id}{}", if active { "*" } else { "" })
                };
                if let (Some(c), Some(r)) = (cols, rows_n) {
                    place.push_str(&format!(" \u{b7} {c}\u{d7}{r}"));
                }
                if let Some(dir) = cwd.filter(|d| !d.is_empty()) {
                    place.push_str(&format!(" \u{b7} {dir}"));
                }
                tabs.push(TabSpreadsheetRow {
                    key: format!("remote/{name}/{inst_pid}/{local_id}"),
                    computer: name.clone(),
                    instance: format!("aterm-{inst_pid}"),
                    place,
                    sid,
                    title,
                    state,
                    program: prog_label,
                    is_ai_cli,
                    ai_status,
                    autopilot,
                    human: human_activity_words(human_ms),
                    vitals: String::new(),
                    show_local_id: None,
                });
            }
        }
        let vitals = hello_json
            .as_ref()
            .and_then(|h| h.get("vitals"))
            .map(|v| format_hello_vitals_json(&uuid, v))
            .unwrap_or_default();
        computers.push(TabComputer {
            id: uuid,
            name,
            address,
            os_version,
            status,
            vitals,
            instances: instance_pids.len(),
            tabs: host_tabs,
            ai_clis: host_ai,
            active_ai: host_active_ai,
        });
    }
    #[cfg(target_os = "macos")]
    discover_bonjour_lan_computers(&mut computers, &custom_names, local_vitals);
    #[cfg(not(target_os = "macos"))]
    let _ = local_vitals;
    (computers, tabs)
}

fn bar_8_gui(pct: u8) -> String {
    let filled = ((u16::from(pct.min(100)) * 8 + 50) / 100) as usize;
    let mut s = String::with_capacity(30);
    s.push('\u{2595}');
    for i in 0..8 {
        s.push(if i < filled { '\u{2588}' } else { '\u{2591}' });
    }
    s.push('\u{258f}');
    s
}

fn human_bytes_gui(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= TIB {
        format!("{:.2} TiB", b / TIB)
    } else if b >= 100.0 * GIB {
        format!("{:.0} GiB", b / GIB)
    } else if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else {
        format!("{:.0} MiB", (b / MIB).max(1.0))
    }
}

fn format_uptime_gui(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        format!("{secs}s")
    }
}

fn push_sparkline_sample(host_key: &str, sample_pct: u8) -> String {
    sparkline(host_key, Some(sample_pct))
}

/// `host_key`'s sparkline as it stands, no sample added.
#[cfg(target_os = "macos")]
fn sparkline_now(host_key: &str) -> String {
    sparkline(host_key, None)
}

/// `host_key`'s last ten samples as a sparkline, after adding `sample_pct` where one is given.
fn sparkline(host_key: &str, sample_pct: Option<u8>) -> String {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Mutex, OnceLock};
    const BLOCKS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    static RINGS: OnceLock<Mutex<BTreeMap<String, VecDeque<u8>>>> = OnceLock::new();
    let mutex = RINGS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
    let q = guard.entry(host_key.to_string()).or_default();
    if let Some(sample_pct) = sample_pct {
        if q.len() >= 10 {
            q.pop_front();
        }
        q.push_back(sample_pct.min(100));
    }
    q.iter()
        .map(|&v| {
            let idx = ((u16::from(v) * 7 + 50) / 100).min(7) as usize;
            BLOCKS[idx]
        })
        .collect()
}

fn format_local_snapshot_vitals(snap: &aterm_vitals::model::Snapshot) -> String {
    let mut parts = Vec::new();
    if let Some(cpu) = &snap.cpu {
        let total = cpu.total().max(1);
        let pct = ((cpu.busy.level / total as f32) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8;
        let spark = push_sparkline_sample("local/cpu", pct);
        let clusters: Vec<String> = cpu
            .clusters
            .iter()
            .map(|c| {
                let c_pct = if c.cores.is_empty() {
                    0.0
                } else {
                    let sum: f32 = c
                        .cores
                        .iter()
                        .filter_map(|&i| cpu.cores.get(i))
                        .map(|cv| cv.load.level)
                        .sum();
                    (sum / c.cores.len() as f32) * 100.0
                };
                format!("{}:{:.0}%", c.name, c_pct)
            })
            .collect();
        let cl_str = if clusters.is_empty() {
            format!("{}c", cpu.total())
        } else {
            format!("{}c \u{b7} {}", cpu.total(), clusters.join(" "))
        };
        parts.push(format!("CPU {pct}% {} {spark} ({cl_str})", bar_8_gui(pct)));
    }
    if let Some(gpu) = &snap.gpu {
        let pct = (gpu.load.level * 100.0).round().clamp(0.0, 100.0) as u8;
        let name = match (gpu.name.as_deref(), gpu.cores) {
            (Some(n), Some(c)) if !n.is_empty() && c > 0 => format!("{n} {c}c"),
            (Some(n), _) if !n.is_empty() => n.to_string(),
            _ => "GPU".to_string(),
        };
        parts.push(format!("GPU {pct}% {} ({name})", bar_8_gui(pct)));
    }
    if let Some(mem) = &snap.mem {
        let used = mem.sample.used();
        let total = mem.sample.total;
        let pct = ((used.saturating_mul(100)) / total.max(1)).min(100) as u8;
        let pressure = match mem.sample.pressure {
            aterm_vitals::model::Pressure::Normal => "normal",
            aterm_vitals::model::Pressure::Warn => "warn",
            aterm_vitals::model::Pressure::Critical => "critical",
            aterm_vitals::model::Pressure::Unknown => "pressure unknown",
        };
        let mut extra = vec![
            pressure.to_string(),
            format!("wired {}", human_bytes_gui(mem.sample.wired)),
        ];
        if let Some(c) = mem.sample.compressed {
            extra.push(format!("comp {}", human_bytes_gui(c)));
        }
        if mem.sample.swap_used > 0 {
            extra.push(format!("swap {}", human_bytes_gui(mem.sample.swap_used)));
        }
        parts.push(format!(
            "RAM {}/{} ({pct}% {} \u{b7} {})",
            human_bytes_gui(used),
            human_bytes_gui(total),
            bar_8_gui(pct),
            extra.join(" \u{b7} ")
        ));
    }
    if let Some(vol) = &snap.volume {
        let used = vol.used();
        let pct = ((used.saturating_mul(100)) / vol.total.max(1)).min(100) as u8;
        parts.push(format!(
            "Disk {}/{} ({pct}% \u{b7} {} free)",
            human_bytes_gui(used),
            human_bytes_gui(vol.total),
            human_bytes_gui(vol.available)
        ));
    }
    parts.join(" \u{b7} ")
}

fn format_hello_vitals_json(host_id: &str, v: &aterm_json::Value) -> String {
    let cores = v.get("cpu_cores").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let cpu_pct = v
        .get("cpu_busy_pct")
        .and_then(|x| x.as_u64())
        .map(|x| x.min(100) as u8);
    let load = v
        .get("load_avg_100")
        .and_then(|x| x.as_array())
        .and_then(|arr| {
            Some([
                arr.first()?.as_u64()? as u32,
                arr.get(1)?.as_u64()? as u32,
                arr.get(2)?.as_u64()? as u32,
            ])
        });
    let mut parts = Vec::new();
    if let Some(pct) = cpu_pct {
        let spark = push_sparkline_sample(&format!("{host_id}/cpu"), pct);
        let load_str = load.map_or_else(String::new, |[l1, l5, l15]| {
            format!(
                " \u{b7} load {:.2}/{:.2}/{:.2}",
                f64::from(l1) / 100.0,
                f64::from(l5) / 100.0,
                f64::from(l15) / 100.0
            )
        });
        parts.push(format!(
            "CPU {pct}% {} {spark} ({cores}c{load_str})",
            bar_8_gui(pct)
        ));
    }
    if let Some(gpu_name) = v
        .get("gpu_name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
        let gpct = v
            .get("gpu_busy_pct")
            .and_then(|x| x.as_u64())
            .map(|x| format!("{}% {} ", x.min(100), bar_8_gui(x.min(100) as u8)))
            .unwrap_or_default();
        parts.push(format!("GPU {gpct}({gpu_name})"));
    }
    let mem_tot = v
        .get("mem_total_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let mem_used = v
        .get("mem_used_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    if mem_tot > 0 {
        let pct = ((mem_used.saturating_mul(100)) / mem_tot.max(1)).min(100) as u8;
        parts.push(format!(
            "RAM {}/{} ({pct}% {})",
            human_bytes_gui(mem_used),
            human_bytes_gui(mem_tot),
            bar_8_gui(pct)
        ));
    }
    let disk_tot = v
        .get("disk_total_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let disk_used = v
        .get("disk_used_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let disk_avail = v
        .get("disk_avail_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    if disk_tot > 0 {
        let pct = ((disk_used.saturating_mul(100)) / disk_tot.max(1)).min(100) as u8;
        parts.push(format!(
            "Disk {}/{} ({pct}% \u{b7} {} free)",
            human_bytes_gui(disk_used),
            human_bytes_gui(disk_tot),
            human_bytes_gui(disk_avail)
        ));
    }
    if let Some(up) = v
        .get("uptime_secs")
        .and_then(|x| x.as_u64())
        .filter(|u| *u > 0)
    {
        parts.push(format!("up {}", format_uptime_gui(up)));
    }
    parts.join(" \u{b7} ")
}

#[cfg(target_os = "macos")]
fn sample_local_vitals_fallback() -> String {
    let layout = aterm_sysprobe::cpu_layout();
    let total_cores = layout.logical_cores() as u32;
    let mut probe = aterm_sysprobe::Probe::new();
    let t1 = probe.core_ticks();
    std::thread::sleep(std::time::Duration::from_millis(30));
    let t2 = probe.core_ticks();
    let cpu_pct = match (t1, t2) {
        (Some(a), Some(b)) if !a.is_empty() && a.len() == b.len() => {
            let mut busy_sum = 0u64;
            let mut tot_sum = 0u64;
            for (x, y) in a.iter().zip(b.iter()) {
                busy_sum = busy_sum.saturating_add(y.busy().saturating_sub(x.busy()));
                tot_sum = tot_sum.saturating_add(y.total().saturating_sub(x.total()));
            }
            busy_sum
                .saturating_mul(100)
                .checked_div(tot_sum)
                .map(|pct| pct.min(100) as u8)
        }
        _ => None,
    };
    let load_str = run_cmd_stdout_bounded("/usr/sbin/sysctl", &["-n", "vm.loadavg"], 60)
        .and_then(|s| parse_loadavg_gui(&s))
        .map(|[l1, l5, l15]| {
            format!(
                "load {:.2}/{:.2}/{:.2}",
                f64::from(l1) / 100.0,
                f64::from(l5) / 100.0,
                f64::from(l15) / 100.0
            )
        });
    let pct = cpu_pct.unwrap_or(8);
    let spark = push_sparkline_sample("local/cpu", pct);
    let mut parts = Vec::new();
    let cl_names: Vec<String> = layout
        .clusters
        .iter()
        .map(|c| format!("{}:{}c", c.name, c.logical.len()))
        .collect();
    let cpu_meta = match (cl_names.is_empty(), load_str) {
        (false, Some(ld)) => {
            format!(
                "{}c [{}] \u{b7} {ld}",
                total_cores.max(1),
                cl_names.join(" ")
            )
        }
        (false, None) => format!("{}c [{}]", total_cores.max(1), cl_names.join(" ")),
        (true, Some(ld)) => format!("{}c \u{b7} {ld}", total_cores.max(1)),
        (true, None) => format!("{}c", total_cores.max(1)),
    };
    parts.push(format!(
        "CPU {pct}% {} {spark} ({cpu_meta})",
        bar_8_gui(pct)
    ));

    if let Some(g) = aterm_sysprobe::gpu() {
        let gname = match (g.name.as_deref(), g.cores) {
            (Some(n), Some(c)) if !n.is_empty() && c > 0 => format!("{n} {c}c"),
            (Some(n), _) if !n.is_empty() => n.to_string(),
            _ => "GPU".to_string(),
        };
        let gpct = g
            .device_pct
            .or((g.unread == Some(aterm_sysprobe::Why::Asleep)).then_some(0));
        match gpct {
            Some(p) => {
                let p = p.min(100);
                parts.push(format!("GPU {p}% {} ({gname})", bar_8_gui(p)));
            }
            None => parts.push(format!(
                "GPU unavailable({}) ({gname})",
                g.unread.unwrap_or(aterm_sysprobe::Why::Unsupported)
            )),
        }
    }
    if let Some(m) = aterm_sysprobe::memory_detail() {
        let used = m
            .app
            .saturating_add(m.shared.unwrap_or(0))
            .saturating_add(m.wired)
            .saturating_add(m.compressed.unwrap_or(0))
            .min(m.total);
        let mpct = ((used.saturating_mul(100)) / m.total.max(1)).min(100) as u8;
        let press = match m.pressure {
            Some(aterm_sysprobe::MemoryLevel::Normal) => "normal",
            Some(aterm_sysprobe::MemoryLevel::Warn) => "warn",
            Some(aterm_sysprobe::MemoryLevel::Critical) => "critical",
            None => "pressure unknown",
        };
        let mut extra = vec![
            press.to_string(),
            format!("wired {}", human_bytes_gui(m.wired)),
        ];
        if let Some(c) = m.compressed {
            extra.push(format!("comp {}", human_bytes_gui(c)));
        }
        if m.swap_used > 0 {
            extra.push(format!("swap {}", human_bytes_gui(m.swap_used)));
        }
        parts.push(format!(
            "RAM {}/{} ({mpct}% {} \u{b7} {})",
            human_bytes_gui(used),
            human_bytes_gui(m.total),
            bar_8_gui(mpct),
            extra.join(" \u{b7} ")
        ));
    }
    if let Some(v) = aterm_sysprobe::volume(std::path::Path::new("/")) {
        let used = v.total.saturating_sub(v.available);
        let dpct = ((used.saturating_mul(100)) / v.total.max(1)).min(100) as u8;
        parts.push(format!(
            "Disk {}/{} ({dpct}% \u{b7} {} free)",
            human_bytes_gui(used),
            human_bytes_gui(v.total),
            human_bytes_gui(v.available)
        ));
    }
    if let Some(boottime) = run_cmd_stdout_bounded("/usr/sbin/sysctl", &["-n", "kern.boottime"], 60)
        .and_then(|s| {
            s.split_once("sec =")?
                .1
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<u64>()
                .ok()
        })
    {
        let now_s = (unix_ms_now() / 1000).max(0) as u64;
        if now_s > boottime {
            parts.push(format!("up {}", format_uptime_gui(now_s - boottime)));
        }
    }
    parts.join(" \u{b7} ")
}

#[cfg(target_os = "macos")]
fn run_cmd_stdout_bounded(prog: &str, args: &[&str], wait_ms: u64) -> Option<String> {
    use std::io::Read;
    let (reader, writer) = std::io::pipe().ok()?;
    let mut cmd = aterm_uds::spawnfd::command(prog);
    cmd.args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::from(writer))
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut child = cmd.spawn().ok()?;
    drop(cmd);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let watcher = std::thread::spawn(move || {
        let _ = done_rx.recv_timeout(std::time::Duration::from_millis(wait_ms));
        let _ = child.kill();
        let _ = child.wait();
    });
    let mut out = String::new();
    let _ = reader.take(32 * 1024).read_to_string(&mut out);
    let _ = done_tx.send(());
    let _ = watcher.join();
    (!out.trim().is_empty()).then_some(out)
}

#[cfg(target_os = "macos")]
fn parse_loadavg_gui(text: &str) -> Option<[u32; 3]> {
    let mut vals = text
        .split(|c: char| c.is_whitespace() || matches!(c, '{' | '}'))
        .filter(|tok| !tok.is_empty())
        .filter_map(|tok| tok.parse::<f64>().ok());
    let l1 = (vals.next()?.max(0.0) * 100.0).round() as u32;
    let l5 = (vals.next()?.max(0.0) * 100.0).round() as u32;
    let l15 = (vals.next()?.max(0.0) * 100.0).round() as u32;
    Some([l1, l5, l15])
}

#[cfg(target_os = "macos")]
fn run_dns_sd_bounded(args: &[&str], wait_ms: u64, stop_substr: Option<&str>) -> String {
    use std::io::{BufRead, BufReader, Read};
    let Ok((reader, writer)) = std::io::pipe() else {
        return String::new();
    };
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/dns-sd");
    cmd.args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::from(writer))
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return String::new();
    };
    drop(cmd);
    let (finished, finish) = std::sync::mpsc::channel();
    let watcher = std::thread::spawn(move || {
        let _ = finish.recv_timeout(std::time::Duration::from_millis(wait_ms));
        let _ = child.kill();
        let _ = child.wait();
    });
    let mut out = String::new();
    let mut buf = BufReader::new(reader.take(64 * 1024));
    loop {
        let mut line = String::new();
        match buf.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if !line.ends_with('\n') {
                    break;
                }
                out.push_str(&line);
                if let Some(needle) = stop_substr
                    && line.contains(needle)
                {
                    break;
                }
            }
        }
    }
    let _ = finished.send(());
    let _ = watcher.join();
    out
}

#[cfg(target_os = "macos")]
fn parse_bonjour_service_instances(text: &str, want_svc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let mut cols = line.split_whitespace();
        let Some(_ts) = cols.next() else { continue };
        let Some(ar) = cols.next() else { continue };
        let Some(_flags) = cols.next() else { continue };
        let Some(_ifx) = cols.next() else { continue };
        let Some(_dom) = cols.next() else { continue };
        let Some(svc) = cols.next() else { continue };
        if svc != want_svc {
            continue;
        }
        let rest: Vec<&str> = cols.collect();
        if rest.is_empty() {
            continue;
        }
        let inst_name = rest.join(" ");
        if ar == "Add" {
            if !out.iter().any(|n| n.eq_ignore_ascii_case(&inst_name)) {
                out.push(inst_name);
            }
        } else if ar == "Rmv" {
            out.retain(|n| !n.eq_ignore_ascii_case(&inst_name));
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn describe_gui_hardware(model_id: Option<&str>) -> String {
    model_id
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("hardware unknown")
        .to_string()
}

/// How long a host's last address, SSH banner and round trip stand in for probes that miss: one
/// slow or lost 120 ms probe on a loaded machine is not news, and a row that dropped them for one
/// sweep reflowed the list under its reader.
#[cfg(any(target_os = "macos", test))]
const HOST_FACTS_HELD: std::time::Duration = std::time::Duration::from_secs(30);

/// What one probe of a host finds: its IPv4 address, its SSH banner and the round trip of the
/// connect that read it ([`resolve_host_ipv4_banner_and_rtt`]).
#[cfg(any(target_os = "macos", test))]
pub(crate) type HostFacts = (Option<String>, Option<String>, Option<u64>);

/// One host's last good probe facts, each with when it was last seen.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeldHostFacts {
    ipv4: Option<(String, Instant)>,
    banner: Option<(String, Instant)>,
    rtt_us: Option<(u64, Instant)>,
}

#[cfg(any(target_os = "macos", test))]
impl HeldHostFacts {
    /// The facts to show after a probe at `now` found `probed` (address, banner, round trip): what
    /// it found, else what was found within [`HOST_FACTS_HELD`], else nothing; and whether the round
    /// trip is a fresh sample (a held one is shown, never sampled again).
    pub(crate) fn after_probe(&mut self, probed: HostFacts, now: Instant) -> (HostFacts, bool) {
        fn hold<T: Clone>(
            slot: &mut Option<(T, Instant)>,
            seen: Option<T>,
            now: Instant,
        ) -> Option<T> {
            match seen {
                Some(value) => {
                    *slot = Some((value.clone(), now));
                    Some(value)
                }
                None => {
                    if slot
                        .as_ref()
                        .is_some_and(|(_, at)| now.saturating_duration_since(*at) > HOST_FACTS_HELD)
                    {
                        *slot = None;
                    }
                    slot.as_ref().map(|(value, _)| value.clone())
                }
            }
        }
        let (ipv4, banner, rtt_us) = probed;
        let fresh_rtt = banner.is_some() && rtt_us.is_some();
        let banner = hold(&mut self.banner, banner, now);
        let rtt_us = hold(&mut self.rtt_us, rtt_us.filter(|_| fresh_rtt), now);
        ((hold(&mut self.ipv4, ipv4, now), banner, rtt_us), fresh_rtt)
    }
}

/// [`resolve_host_ipv4_banner_and_rtt`] through each host's [`HeldHostFacts`].
#[cfg(target_os = "macos")]
fn probe_host_facts(hostname: &str, port: u16) -> (HostFacts, bool) {
    type ByHost = std::collections::BTreeMap<(String, u16), HeldHostFacts>;
    static HELD: std::sync::OnceLock<std::sync::Mutex<ByHost>> = std::sync::OnceLock::new();
    let probed = resolve_host_ipv4_banner_and_rtt(hostname, port);
    let hosts = HELD.get_or_init(Default::default);
    let mut held = hosts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.entry((hostname.to_string(), port))
        .or_default()
        .after_probe(probed, Instant::now())
}

#[cfg(target_os = "macos")]
fn resolve_host_ipv4_banner_and_rtt(hostname: &str, port: u16) -> HostFacts {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    let ipv4 = (hostname, port).to_socket_addrs().ok().and_then(|addrs| {
        let v4s: Vec<std::net::Ipv4Addr> = addrs
            .filter_map(|a| match a {
                SocketAddr::V4(v4) => Some(*v4.ip()),
                SocketAddr::V6(_) => None,
            })
            .collect();
        v4s.iter()
            .copied()
            .find(|ip| !ip.is_loopback() && !ip.is_unspecified())
            .or_else(|| v4s.first().copied())
            .map(|ip| ip.to_string())
    });
    let mut rtt_us = None;
    let banner = ipv4
        .as_deref()
        .and_then(|ip| ip.parse::<std::net::Ipv4Addr>().ok())
        .and_then(|ip| {
            let addr = SocketAddr::from((ip, port));
            let t0 = Instant::now();
            let stream =
                TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(120)).ok()?;
            rtt_us = u64::try_from(t0.elapsed().as_micros().max(1)).ok();
            read_ssh_banner(
                &stream,
                Instant::now() + std::time::Duration::from_millis(120),
            )
        });
    (ipv4, banner, rtt_us)
}

#[cfg(target_os = "macos")]
fn read_ssh_banner(mut stream: &std::net::TcpStream, deadline: Instant) -> Option<String> {
    use std::io::Read;
    let mut bytes = [0; 255];
    let mut filled = 0;
    while filled < bytes.len() {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(remaining)).ok()?;
        let read = stream.read(&mut bytes[filled..]).ok()?;
        if read == 0 {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        if let Some(newline) = bytes[filled..filled + read]
            .iter()
            .position(|b| *b == b'\n')
        {
            let end = filled + newline;
            return std::str::from_utf8(&bytes[..=end])
                .ok()?
                .trim()
                .strip_prefix("SSH-2.0-")
                .filter(|banner| !banner.is_empty())
                .map(str::to_owned);
        }
        filled += read;
    }
    None
}

#[cfg(target_os = "macos")]
fn bonjour_ssh_target(text: &str) -> Option<(String, u16)> {
    for line in text.lines() {
        let Some((_, rest)) = line.split_once(" can be reached at ") else {
            continue;
        };
        let hostport = rest.split(" (interface ").next()?.trim();
        let Some((host, port)) = hostport.rsplit_once(':') else {
            continue;
        };
        let host = host.trim_end_matches('.');
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        if !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Some((host.to_owned(), port));
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn parse_bonjour_device_txt(text: &str) -> (Option<String>, Option<u32>) {
    let mut decoded = String::from(text);
    for line in text.lines() {
        if let Some((_, hex_part)) = line.split_once("bytes:") {
            decoded.push(' ');
            for tok in hex_part.split_whitespace() {
                if tok.len() == 2
                    && let Ok(b) = u8::from_str_radix(tok, 16)
                {
                    if (0x21..=0x7e).contains(&b) {
                        decoded.push(b as char);
                    } else {
                        decoded.push(' ');
                    }
                }
            }
        }
    }
    let mut model = None;
    let mut osxvers = None;
    for tok in decoded.split(|c: char| c.is_whitespace() || c == '\\') {
        if let Some(m) = tok.strip_prefix("model=") {
            let clean: String = m
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == ',')
                .collect();
            if !clean.is_empty() {
                model = Some(clean);
            }
        } else if let Some(v) = tok.strip_prefix("osxvers=") {
            let digits: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                osxvers = Some(n);
            }
        }
    }
    (model, osxvers)
}

#[cfg(target_os = "macos")]
type BonjourResolvedEntry = (String, u16, Option<String>, Option<u32>);

#[cfg(target_os = "macos")]
type BonjourResolvedMap = std::collections::BTreeMap<String, BonjourResolvedEntry>;

#[cfg(target_os = "macos")]
struct CachedBonjourBrowse {
    at: Instant,
    lan_names: Vec<String>,
    sftp_names: Vec<String>,
    rfb_names: Vec<String>,
    resolved: BonjourResolvedMap,
}

#[cfg(target_os = "macos")]
fn cached_bonjour_browse() -> (Vec<String>, Vec<String>, Vec<String>, BonjourResolvedMap) {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<CachedBonjourBrowse>>> =
        std::sync::OnceLock::new();
    let mutex = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(guard) = mutex.lock()
        && let Some(c) = guard.as_ref()
        && c.at.elapsed() < std::time::Duration::from_secs(30)
    {
        return (
            c.lan_names.clone(),
            c.sftp_names.clone(),
            c.rfb_names.clone(),
            c.resolved.clone(),
        );
    }
    let (ssh_txt, sftp_txt, rfb_txt) = std::thread::scope(|s| {
        let h1 = s.spawn(|| run_dns_sd_bounded(&["-B", "_ssh._tcp", "local."], 140, None));
        let h2 = s.spawn(|| run_dns_sd_bounded(&["-B", "_sftp-ssh._tcp", "local."], 140, None));
        let h3 = s.spawn(|| run_dns_sd_bounded(&["-B", "_rfb._tcp", "local."], 140, None));
        (
            h1.join().unwrap_or_default(),
            h2.join().unwrap_or_default(),
            h3.join().unwrap_or_default(),
        )
    });
    let mut lan_names = parse_bonjour_service_instances(&ssh_txt, "_ssh._tcp.");
    let sftp_names = parse_bonjour_service_instances(&sftp_txt, "_sftp-ssh._tcp.");
    let rfb_names = parse_bonjour_service_instances(&rfb_txt, "_rfb._tcp.");
    lan_names.sort_by_key(|n| natural_lan_sort_key(n));
    let resolved_pairs: Vec<(String, BonjourResolvedEntry)> = std::thread::scope(|s| {
        let handles: Vec<_> = lan_names
            .iter()
            .map(|inst_name| {
                s.spawn(move || {
                    let resolve_out = run_dns_sd_bounded(
                        &["-L", inst_name, "_ssh._tcp", "local."],
                        100,
                        Some("can be reached at"),
                    );
                    let (dot_local, port) = bonjour_ssh_target(&resolve_out)?;
                    let txt_q = format!("{inst_name}._device-info._tcp.local.");
                    let txt_out = run_dns_sd_bounded(&["-Q", &txt_q, "TXT"], 80, Some("bytes:"));
                    let (model_id, osxvers) = parse_bonjour_device_txt(&txt_out);
                    Some((inst_name.clone(), (dot_local, port, model_id, osxvers)))
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    });
    let resolved: std::collections::BTreeMap<_, _> = resolved_pairs.into_iter().collect();
    if let Ok(mut guard) = mutex.lock() {
        *guard = Some(CachedBonjourBrowse {
            at: Instant::now(),
            lan_names: lan_names.clone(),
            sftp_names: sftp_names.clone(),
            rfb_names: rfb_names.clone(),
            resolved: resolved.clone(),
        });
    }
    (lan_names, sftp_names, rfb_names, resolved)
}

#[cfg(target_os = "macos")]
fn discover_bonjour_lan_computers(
    computers: &mut Vec<TabComputer>,
    custom_names: &std::collections::BTreeMap<String, String>,
    local_vitals: bool,
) {
    let (lan_names, sftp_names, rfb_names, resolved_map) = cached_bonjour_browse();

    let raw_local = raw_local_host_name_gui();
    let local_canonical = canonical_host_name_gui(&raw_local, custom_names);
    let local_dot = format!("{raw_local}.local");
    let ((local_ip, local_ssh, _), _) = probe_host_facts(&local_dot, 22);
    let local_addr = match local_ip {
        Some(ip) => format!("{ip} \u{b7} {local_dot}"),
        None => local_dot.clone(),
    };
    let (local_model, local_osx) = resolved_map
        .get(&raw_local.replace('-', " "))
        .or_else(|| resolved_map.get(&raw_local))
        .map(|(_, _, m, o)| (m.clone(), *o))
        .unwrap_or_else(|| {
            let local_txt_q = format!("{}._device-info._tcp.local.", raw_local.replace('-', " "));
            let local_txt = run_dns_sd_bounded(&["-Q", &local_txt_q, "TXT"], 80, Some("bytes:"));
            parse_bonjour_device_txt(&local_txt)
        });
    let local_hw = describe_gui_hardware(local_model.as_deref());
    let local_os_str = match local_osx {
        Some(d) => format!("macOS (Darwin {d})"),
        None => "macOS version unknown".to_string(),
    };
    let local_ssh_str = local_ssh
        .as_deref()
        .and_then(|b| b.split_whitespace().next())
        .unwrap_or("SSH banner unknown");
    let local_services: Vec<&str> = [
        ("ssh", &lan_names),
        ("sftp", &sftp_names),
        ("screen-sharing", &rfb_names),
    ]
    .into_iter()
    .filter_map(|(service, names)| {
        names
            .iter()
            .any(|name| {
                canonical_host_name_gui(name, custom_names).eq_ignore_ascii_case(&local_canonical)
            })
            .then_some(service)
    })
    .collect();
    let local_services = if local_services.is_empty() {
        "none observed".to_string()
    } else {
        local_services.join(", ")
    };
    computers.push(TabComputer {
        id: "__local_meta__".to_string(),
        name: format!("{local_canonical} (this Mac)"),
        address: local_addr,
        os_version: format!(
            "{local_hw} \u{b7} {local_os_str} \u{b7} {local_ssh_str} \u{b7} advertised services: {local_services}"
        ),
        status: "online (local)".to_string(),
        vitals: if local_vitals {
            sample_local_vitals_fallback()
        } else {
            String::new()
        },
        instances: 0,
        tabs: 0,
        ai_clis: 0,
        active_ai: 0,
    });

    let candidates: Vec<String> = lan_names
        .into_iter()
        .filter(|inst_name| {
            let canonical = canonical_host_name_gui(inst_name, custom_names);
            if canonical.eq_ignore_ascii_case(&local_canonical) {
                return false;
            }
            !computers.iter().any(|c| {
                c.id != "__local_meta__"
                    && (c.name.eq_ignore_ascii_case(&canonical)
                        || c.name.eq_ignore_ascii_case(inst_name))
            })
        })
        .collect();

    let results: Vec<TabComputer> = std::thread::scope(|s| {
        let handles: Vec<_> = candidates
            .iter()
            .map(|inst_name| {
                let sftp_ref = &sftp_names;
                let rfb_ref = &rfb_names;
                let resolved_ref = &resolved_map;
                s.spawn(move || {
                    let canonical = canonical_host_name_gui(inst_name, custom_names);
                    let (dot_local, port, model_id, osxvers) =
                        resolved_ref.get(inst_name)?.clone();
                    let ((ipv4, ssh_banner, rtt_us), fresh_rtt) =
                        probe_host_facts(&dot_local, port);
                    let addr_display = match ipv4 {
                        Some(ip) => format!("{ip} \u{b7} {dot_local}"),
                        None => dot_local.clone(),
                    };
                    let has_sftp = sftp_ref.iter().any(|x| x.eq_ignore_ascii_case(inst_name));
                    let has_rfb = rfb_ref.iter().any(|x| x.eq_ignore_ascii_case(inst_name));
                    let mut svcs = vec!["ssh"];
                    if has_sftp {
                        svcs.push("sftp");
                    }
                    if has_rfb {
                        svcs.push("screen-sharing");
                    }
                    let hw_display = describe_gui_hardware(model_id.as_deref());
                    let os_str = osxvers.map_or_else(
                        || "OS version unknown".to_string(),
                        |d| format!("Darwin {d}"),
                    );
                    let ssh_short = ssh_banner
                        .as_deref()
                        .and_then(|b| b.split_whitespace().next())
                        .unwrap_or("SSH banner unknown");
                    let os_full = format!(
                        "{hw_display} \u{b7} {os_str} \u{b7} {ssh_short} \u{b7} advertised services: {}",
                        svcs.join(", ")
                    );
                    let vitals = if ssh_banner.is_some()
                        && let Some(us) = rtt_us
                    {
                        let rtt_pct = ((us / 500).min(100)) as u8;
                        let rtt_key = format!("lan/{canonical}/rtt");
                        let rtt_spark = if fresh_rtt {
                            push_sparkline_sample(&rtt_key, rtt_pct)
                        } else {
                            sparkline_now(&rtt_key)
                        };
                        format!(
                            "LAN SSH RTT {:.1}ms {rtt_spark} \u{b7} system vitals unobserved (enroll in aterm machines for CPU/GPU/RAM/Disk)",
                            us as f64 / 1_000.0
                        )
                    } else {
                        String::new()
                    };
                    Some(TabComputer {
                        id: format!("lan/{canonical}"),
                        name: canonical,
                        address: addr_display,
                        os_version: os_full,
                        status: if ssh_banner.is_some() {
                            "reachable (ssh)".into()
                        } else {
                            "advertised (lan)".into()
                        },
                        vitals,
                        instances: 0,
                        tabs: 0,
                        ai_clis: 0,
                        active_ai: 0,
                    })
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    });

    for comp in results {
        computers.push(comp);
    }
}

#[cfg(target_os = "macos")]
fn natural_lan_sort_key(name: &str) -> (u8, u64, String) {
    let trimmed = name.trim();
    if let Some(rest) = trimmed
        .strip_prefix('m')
        .or_else(|| trimmed.strip_prefix('M'))
    {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = digits.parse::<u64>() {
            return (0, num, trimmed.to_ascii_lowercase());
        }
    }
    (1, 0, trimmed.to_ascii_lowercase())
}

/// Unix milliseconds now (the wall clock: the ledgers' rows are stamped on it, and a task's face is stamped with it, `App::apply_harness_fact`).
pub(crate) fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "harness_screen_tests.rs"]
mod tests;
