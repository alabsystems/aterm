// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OVERVIEW'S "ON THE FABRIC" GROUP: what the fabric's log says the sessions on this aterm's fabric's OTHER machines need of the person — a UX decision made from the stream alone, readable with `controls overview`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::overview::{ChipMark, FabricChip};

/// One read's kill: the brief bounds its dial and every read and write at 3 s (`aterm_link::fabric::IO_TIMEOUT`), so this is the cap, never a wait it is expected to reach.
pub(crate) const READ_CAP: Duration = Duration::from_secs(10);

/// The budget the read asks for: the brief's ceiling (its `--budget` bound),
/// so every session the fold holds fits the answer.
const READ_BUDGET: &str = "100000";

/// The most needs one snapshot keeps (the brief itself holds at most 1024
/// sessions); the rest are not listed.
const NEEDS_MAX: usize = 1024;

/// One session on another machine whose fold names an observed need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FabricNeed {
    /// Its node id.
    pub(crate) node: String,
    /// Its face on the bus.
    pub(crate) sid: String,
    /// The need word (`approval:bash`).
    pub(crate) need: String,
    /// The need's detail (a wall's reset, a failure's exit code).
    pub(crate) detail: Option<String>,
    /// How old the fold's newest record was at the read, in seconds; `None` when the brief could not say.
    pub(crate) age_s: Option<u64>,
    /// Its node is gone (the brief's `partial` holds `node-gone`): the session ended without its close, and nobody can answer it there.
    pub(crate) gone: bool,
}

/// One read of the fabric's log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FabricSnapshot {
    /// When it was read (unix ms).
    pub(crate) read_at_ms: u64,
    /// The needs, in the brief's order (newest first).
    pub(crate) needs: Vec<FabricNeed>,
}

/// WHY A READ FAILED, in a closed word: no text of the child's (its stderr can name a path) is kept, logged or shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadFail {
    /// The child did not start, or did not answer before [`READ_CAP`].
    Run,
    /// It answered with a non-zero exit or nothing at all (no bridge, no broker, a refusal).
    Refused,
    /// Its answer was not the brief's object.
    NoBrief,
}

impl ReadFail {
    /// Its word, as the group's header and `controls overview` say it.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Refused => "refused",
            Self::NoBrief => "no-brief",
        }
    }
}

/// A word field of a brief object, bounded as the brief bounds it: `None`
/// for an empty one or one holding a space or a control character, which no
/// node id, face or need word holds (the brief escapes what the bus sent,
/// and a word with a space would split `controls overview`'s `key=value`
/// line).
fn word(obj: &aterm_json::Map, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(aterm_json::Value::as_str)
        .filter(|w| !w.is_empty() && !w.chars().any(|c| c.is_whitespace() || c.is_control()))
        .map(|w| crate::needs::fold_clip(w, 64))
}

/// THE READ, PARSED: `json` is `aterm fabric brief --json`'s answer, read at `now_ms`.
pub(crate) fn parse(json: &str, now_ms: u64) -> Result<FabricSnapshot, ReadFail> {
    let value: aterm_json::Value = aterm_json::from_str(json).map_err(|_| ReadFail::NoBrief)?;
    let sessions = value
        .get("sessions")
        .and_then(aterm_json::Value::as_array)
        .ok_or(ReadFail::NoBrief)?;
    let needs = sessions
        .iter()
        .filter_map(aterm_json::Value::as_object)
        .filter(|s| s.get("this").and_then(aterm_json::Value::as_bool) != Some(true))
        .filter_map(|s| {
            Some(FabricNeed {
                node: word(s, "node")?,
                sid: word(s, "sid")?,
                need: word(s, "need")?,
                detail: word(s, "detail"),
                age_s: s.get("age_s").and_then(aterm_json::Value::as_u64),
                gone: s
                    .get("partial")
                    .and_then(aterm_json::Value::as_array)
                    .is_some_and(|why| why.iter().any(|w| w.as_str() == Some("node-gone"))),
            })
        })
        .take(NEEDS_MAX)
        .collect();
    Ok(FabricSnapshot {
        read_at_ms: now_ms,
        needs,
    })
}

/// The chip mark a remote need takes: ◇ for a box or a question (the person
/// decides), ◆ for a failure, a wall or a session not responding (it is
/// stuck); `None` for a word the group lists no chip for.
fn mark_of(need: &str) -> Option<ChipMark> {
    match need {
        w if w.starts_with("approval:") => Some(ChipMark::Decide),
        "question" => Some(ChipMark::Decide),
        "failure" | "unresponsive" => Some(ChipMark::Stuck),
        w if w.starts_with("wall:") => Some(ChipMark::Stuck),
        _ => None,
    }
}

/// THE GROUP, COMPOSED at `now_ms`: one chip per need the group lists ([`mark_of`]), in the clause's own words (`aterm_slog::need_words`), its age the fold's at the read plus the time since (shown only).
pub(crate) fn compose(snapshot: &FabricSnapshot, now_ms: u64) -> Vec<FabricChip> {
    let read_age_ms = now_ms.saturating_sub(snapshot.read_at_ms);
    let since_read = read_age_ms / 1000;
    let stale = u128::from(read_age_ms) > crate::fleet_glance::COUNT_FRESH.as_millis();
    snapshot
        .needs
        .iter()
        .filter_map(|n| {
            let mark = mark_of(&n.need)?;
            let words = aterm_slog::need_words(&n.need, n.detail.as_deref())
                .map_or_else(|| n.need.clone(), |(w, _)| w.as_str().to_string());
            Some(FabricChip {
                node: n.node.clone(),
                sid: n.sid.clone(),
                mark,
                need: n.need.clone(),
                words,
                age_s: n.age_s.map(|a| a.saturating_add(since_read)),
                older: stale || n.gone,
            })
        })
        .collect()
}

/// What a read answered: the child's JSON, or why there is none.
pub(crate) type ReadAnswer = Result<String, ReadFail>;

/// The read itself, injected so a test drives the thread with no child.
pub(crate) trait FabricRunner: Send + Sync {
    /// Read the fabric's log once.
    fn run(&self) -> ReadAnswer;
}

/// THE REAL READ: `<program> <args>` as a child, SIGKILLed at `cap`.
pub(crate) struct ChildRunner {
    /// The `aterm` binary (`ledger_key::aterm_cli`).
    pub(crate) program: PathBuf,
    /// `fabric brief --json --budget 100000`.
    pub(crate) args: Vec<String>,
    /// The kill.
    pub(crate) cap: Duration,
}

impl ChildRunner {
    /// The read the Overview makes.
    pub(crate) fn brief() -> Self {
        Self {
            program: crate::ledger_key::aterm_cli(),
            args: ["fabric", "brief", "--json", "--budget", READ_BUDGET]
                .map(str::to_string)
                .to_vec(),
            cap: READ_CAP,
        }
    }
}

impl FabricRunner for ChildRunner {
    fn run(&self) -> ReadAnswer {
        let ran = crate::bounded_child::run_bounded(&self.program, &self.args, Some(self.cap))
            .map_err(|_| ReadFail::Run)?;
        if ran.code != Some(0) || ran.stdout.trim().is_empty() {
            return Err(ReadFail::Refused);
        }
        Ok(ran.stdout)
    }
}

/// Whether this process's bridge supervisor names a command: armed (an
/// `aterm fabric on`'s attach) or configured (`[fabric] command`).
#[cfg(all(not(test), unix))]
fn bridge_named() -> bool {
    let s = crate::fabric_launch::status();
    s.armed.is_some() || s.configured.is_some()
}

/// Off unix there is no bridge.
#[cfg(all(not(test), not(unix)))]
fn bridge_named() -> bool {
    false
}

/// The inert read a test gets unless it names one: no child, no brief.
#[cfg(test)]
struct NoRead;

#[cfg(test)]
impl FabricRunner for NoRead {
    fn run(&self) -> ReadAnswer {
        Err(ReadFail::Run)
    }
}

/// The shared slot the read writes and the card reads.
#[derive(Clone, Debug, Default)]
pub(crate) struct FabricSlot {
    /// The last good read.
    pub(crate) snapshot: Option<Arc<FabricSnapshot>>,
    /// Why the last read failed, when it did (cleared by a good one).
    pub(crate) error: Option<ReadFail>,
    /// A read is out.
    pub(crate) reading: bool,
    /// How many reads ended, good or not.
    pub(crate) reads: u64,
}

/// Own one claimed read until it answers, fails to spawn, or unwinds.
struct ReadCompletion {
    slot: Arc<Mutex<FabricSlot>>,
    proxy: Option<aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
    answer: Option<Result<Arc<FabricSnapshot>, ReadFail>>,
}

impl ReadCompletion {
    /// Finish through this owner, which publishes the answer as it drops.
    fn complete(mut self, answer: Result<Arc<FabricSnapshot>, ReadFail>) {
        self.answer = Some(answer);
        drop(self);
    }
}

impl Drop for ReadCompletion {
    fn drop(&mut self) {
        {
            let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            slot.reading = false;
            slot.reads = slot.reads.saturating_add(1);
            match self.answer.take().unwrap_or(Err(ReadFail::Run)) {
                Ok(snapshot) => {
                    slot.snapshot = Some(snapshot);
                    slot.error = None;
                }
                Err(why) => slot.error = Some(why),
            }
        }
        if let Some(proxy) = &self.proxy {
            let _ = proxy.send_event(crate::Wake::FabricRead);
        }
    }
}

/// The App's side of the group: the slot, and the test arms.
#[derive(Default)]
pub(crate) struct FabricGlance {
    /// The shared slot.
    pub(crate) slot: Arc<Mutex<FabricSlot>>,
    /// TEST ARM: the read.
    #[cfg(test)]
    pub(crate) runner: Option<Arc<dyn FabricRunner>>,
    /// TEST ARM: whether this aterm's fabric is configured.
    #[cfg(test)]
    pub(crate) configured: bool,
    /// When the last read began (main thread): an open card reads again a step after it ([`crate::App::overview_fabric_wake`]).
    pub(crate) kicked_at: Option<Instant>,
    /// TEST RECORD: how many reads were started.
    #[cfg(test)]
    pub(crate) started: u64,
    /// TEST ARM: the wall clock (unix ms) the read is stamped with and the chips are composed at, so freshness is decided on hand-set stamps.
    #[cfg(test)]
    pub(crate) clock_ms: Option<u64>,
}

impl std::fmt::Debug for FabricGlance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FabricGlance")
            .field("slot", &self.copy())
            .finish_non_exhaustive()
    }
}

impl FabricGlance {
    /// The slot as it stands, copied out under its lock (poison recovered: plain data, the snapshot shared).
    pub(crate) fn copy(&self) -> FabricSlot {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The read a kick runs: the test's own, or the inert one.
    #[cfg(test)]
    fn runner(&self) -> Arc<dyn FabricRunner> {
        self.runner
            .clone()
            .unwrap_or_else(|| Arc::new(NoRead) as Arc<dyn FabricRunner>)
    }

    /// The read a kick runs: the `aterm fabric brief --json` child.
    #[cfg(not(test))]
    fn runner(&self) -> Arc<dyn FabricRunner> {
        Arc::new(ChildRunner::brief())
    }

    /// The clock a read is stamped with and the chips composed at: the
    /// test's hand-set stamp, else the wall clock.
    #[cfg(test)]
    fn clock(&self) -> Option<u64> {
        self.clock_ms
    }

    /// The clock a read is stamped with and the chips composed at: the wall
    /// clock.
    #[cfg(not(test))]
    #[allow(clippy::unused_self, reason = "the test arm reads its own stamp")]
    const fn clock(&self) -> Option<u64> {
        None
    }

    /// Whether a read is out.
    fn reading(&self) -> bool {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).reading
    }
}

/// Now in unix ms: `clock` where a test set it, else the wall clock.
fn now_ms(clock: Option<u64>) -> u64 {
    clock.unwrap_or_else(|| aterm_time::unix_millis().unwrap_or(0))
}

impl crate::App {
    /// Whether this aterm's fabric is configured: its bridge supervisor names a command ([`bridge_named`]).
    #[cfg(test)]
    pub(crate) fn fabric_configured(&self) -> bool {
        self.agents.overview_fabric.configured
    }

    /// Whether this aterm's fabric is configured: its bridge supervisor names a command ([`bridge_named`]).
    #[cfg(not(test))]
    pub(crate) fn fabric_configured(&self) -> bool {
        bridge_named()
    }

    /// KICK ONE READ of the fabric's log where the fabric is configured and none is out: on thread `aterm-fabric-brief`, which writes the slot and posts `Wake::FabricRead`.
    pub(crate) fn kick_fabric_read(&mut self) {
        if !self.fabric_configured() {
            self.agents.overview_fabric.kicked_at = None;
            return;
        }
        let glance = &self.agents.overview_fabric;
        let completion = {
            let mut slot = glance.slot.lock().unwrap_or_else(|p| p.into_inner());
            if slot.reading {
                return;
            }
            let completion = ReadCompletion {
                slot: Arc::clone(&glance.slot),
                proxy: self.proxy.clone(),
                answer: None,
            };
            slot.reading = true;
            completion
        };
        let runner = glance.runner();
        let clock = glance.clock();
        self.agents.overview_fabric.kicked_at = Some(Instant::now());
        #[cfg(test)]
        {
            self.agents.overview_fabric.started += 1;
        }
        let _ = crate::qos::spawn_lane_thread(
            "aterm-fabric-brief",
            crate::qos::Role::Background,
            move || {
                completion.complete(
                    runner
                        .run()
                        .and_then(|json| parse(&json, now_ms(clock)))
                        .map(Arc::new),
                );
            },
        );
    }

    /// THE MINUTE'S WAKE (the event loop's `WaitUntil`, owner `overview_fabric`): a step (`OVERVIEW_AGE_STEP`, a minute) after the last read began, while an Overview is open on some window and no read is out.
    pub(crate) fn overview_fabric_wake(&self) -> Option<Instant> {
        let at = self.agents.overview_fabric.kicked_at?;
        if !self.windows.values().any(|ws| ws.overview().is_some())
            || self.agents.overview_fabric.reading()
        {
            return None;
        }
        Some(at + crate::app_overview::OVERVIEW_AGE_STEP)
    }

    /// The minute's wake, come: an open card reads the fabric's log again ([`Self::kick_fabric_read`]) once [`Self::overview_fabric_wake`] is at or before `now`.
    pub(crate) fn tick_overview_fabric(&mut self, now: Instant) {
        if self.overview_fabric_wake().is_some_and(|at| at <= now) {
            self.kick_fabric_read();
        }
    }

    /// The fabric group now, from the last good read, and why the last read failed where it did: nothing where the fabric is off or nothing was read yet.
    pub(crate) fn overview_fabric_group(&self) -> crate::overview::FabricGroup {
        if !self.fabric_configured() {
            return crate::overview::FabricGroup::default();
        }
        let glance = &self.agents.overview_fabric;
        let slot = glance.copy();
        crate::overview::FabricGroup {
            chips: slot
                .snapshot
                .map_or_else(Vec::new, |s| compose(&s, now_ms(glance.clock()))),
            failed: slot.error.map(ReadFail::word),
        }
    }

    /// The `Wake::FabricRead` arm: a read of the fabric's log landed, so every open card is re-rendered with it.
    pub(crate) fn on_fabric_read(&mut self) {
        self.overview_refresh_all();
    }
}

#[cfg(test)]
#[path = "overview_fabric_tests.rs"]
mod tests;
