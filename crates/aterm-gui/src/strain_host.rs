// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STRAIN HOST — the GUI's glue for explaining very heavy system use on the
//! band (design §10.14, rulings 206–212).
//!
//! The policy is `aterm_messages::strain` (pure, clockless, platform-free) and
//! the samplers are `aterm-sysprobe` (every platform `cfg`). This module only
//! plumbs them into the loop, and adds no platform `cfg` of its own:
//!
//! * **Samples.** `metrics::record_present` hands back a HARDWARE key's
//!   closed, untainted input→present slice ([`App::note_strain_key`]): one ring
//!   write on a frame that is drawn anyway. A control `send`/`feed`/`key`
//!   never arms a hardware stamp, and a key typed into a REMOTE program
//!   ([`REMOTE_PROGRAMS`] — a slow network is not a slow Mac) is dropped here.
//!   The watchdog's turn census hands over every turn of at least `FREEZE_MS`
//!   ([`crate::watchdog::take_freeze`]); one that ends within
//!   `TYPING_HOT_TAIL` of a hardware key is a hitch.
//! * **Readings.** The `aterm-strain-probe` thread starts lazily at the first
//!   reading an episode asks for, runs at QoS utility and BLOCKS on its
//!   channel: it never wakes itself. It answers each request with one
//!   `Wake::StrainReading`.
//! * **The deadline.** `DeadlineOwner::SystemStrain` (slot 40), folded after
//!   the band's motion: `Some` only while the engine is Suspect or Open, the
//!   `explain_heavy_load` switch is on, a focused window's band is on screen
//!   ([`App::strain_gate`]) and no reading is in flight.
//! * **The idle law.** Calm arms nothing. An unfocused, occluded, minimized or
//!   headless window takes no samples and arms nothing, so an episode falls
//!   back to calm by itself. A fold leaves nothing armed. The switch off parks
//!   the engine, withdraws the row and drops the thread.
//!
//! # Dev seam
//!
//! `ATERM_DEBUG_STRAIN=cpu|memory|heat` (read once, through `dev_seam!`: a
//! shipped binary never reads it) fakes a saturated reading
//! of that kind on the probe thread and reads every hardware key as slow, so
//! an episode opens under a finger — for captures and demos, never a user
//! setting. The live gate drives real keys with `aterm ctl hwkey`, whose
//! `NSEvent` takes the same `KeyboardInput` arm a physical key does; no test
//! seam makes a control `send` count.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use aterm_messages::MessageId;
use aterm_messages::strain::{
    Gate, ProcRow, Reading, SessionRef, StrainConfig, StrainOut, StrainTracker,
};
use winit::event_loop::EventLoopProxy;

use crate::messages_host::{band_on_screen, restatement_of};
use crate::{App, Wake, WindowId, tab_model};

/// Foreground programs whose keys are never strain samples: their echo rides
/// a network, and a slow network is not a slow machine (ruling 206).
pub(crate) const REMOTE_PROGRAMS: [&str; 6] =
    ["ssh", "mosh-client", "et", "telnet", "kubectl", "docker"];

/// Whether `program` (a session's published foreground program) is remote.
pub(crate) fn is_remote_program(program: &str) -> bool {
    REMOTE_PROGRAMS.contains(&program)
}

/// A turn this near a hardware key is a hitch the person felt.
const FREEZE_NEAR_KEY: Duration = Duration::from_nanos(crate::metrics::TYPING_HOT_TAIL_NS);

/// A reading unanswered this long is lost (the probe thread died): the host
/// forgets it and may ask again.
const LOST_READING: Duration = Duration::from_secs(10);

/// Freezes waiting for a nearby key, at most.
const PENDING_FREEZES: usize = 4;

// ---- `ctl metrics` -------------------------------------------------------

/// The engine's state word for `ctl metrics strain=`: 0 calm, 1 suspect,
/// 2 open, 3 off (the switch).
static STATE: AtomicU8 = AtomicU8::new(0);
/// The slowest reading and the slowest sweep since the last `metrics reset`,
/// in microseconds, as the probe thread measured them.
static PROBE_US_MAX: AtomicU64 = AtomicU64::new(0);
static SCAN_US_MAX: AtomicU64 = AtomicU64::new(0);

fn state_word() -> &'static str {
    match STATE.load(Ordering::Relaxed) {
        1 => "suspect",
        2 => "open",
        3 => "off",
        _ => "calm",
    }
}

/// Clear the probe's cost maxima. Called by [`crate::metrics::reset`], so they
/// are window stats like every other `_max` on the line.
pub(crate) fn reset_metrics() {
    PROBE_US_MAX.store(0, Ordering::Relaxed);
    SCAN_US_MAX.store(0, Ordering::Relaxed);
}

/// The strain fields of the `metrics` summary, text form (one fragment, the
/// `turn_census_fields_text` discipline).
#[must_use]
pub(crate) fn metrics_fields_text() -> String {
    format!(
        " strain={} strain_probe_us_max={} strain_scan_us_max={}",
        state_word(),
        PROBE_US_MAX.load(Ordering::Relaxed),
        SCAN_US_MAX.load(Ordering::Relaxed),
    )
}

/// Field-for-field JSON twin of [`metrics_fields_text`], with a leading comma.
#[must_use]
pub(crate) fn metrics_fields_json() -> String {
    format!(
        ",\"strain\":\"{}\",\"strain_probe_us_max\":{},\"strain_scan_us_max\":{}",
        state_word(),
        PROBE_US_MAX.load(Ordering::Relaxed),
        SCAN_US_MAX.load(Ordering::Relaxed),
    )
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

// ---- the dev seam ----------------------------------------------------------

/// What `ATERM_DEBUG_STRAIN` fakes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DebugLoad {
    Cpu,
    Memory,
    Heat,
}

impl DebugLoad {
    fn parse(word: &str) -> Option<Self> {
        match word.trim() {
            "cpu" => Some(Self::Cpu),
            "memory" => Some(Self::Memory),
            "heat" => Some(Self::Heat),
            _ => None,
        }
    }
}

fn debug_load() -> Option<DebugLoad> {
    static LOAD: OnceLock<Option<DebugLoad>> = OnceLock::new();
    *LOAD.get_or_init(|| {
        // A development seam: compiled out of a shipped binary (`dev_seam!`),
        // denied at the hop to a nested aterm (`ENV_DENY_VARS`).
        aterm_types::dev_seam!("ATERM_DEBUG_STRAIN")
            .and_then(|w| DebugLoad::parse(&w.to_string_lossy()))
    })
}

/// The dev seam's fake counters: cumulative, so the engine's deltas read a
/// saturated machine.
#[derive(Debug, Default)]
struct Saturate {
    ticks: u64,
    swap: u64,
}

impl Saturate {
    fn apply(&mut self, load: DebugLoad, r: &mut Reading) {
        use aterm_messages::strain::{MemoryLevel, Thermal};
        match load {
            DebugLoad::Cpu => {
                self.ticks += 100_000;
                r.busy_ticks = Some((self.ticks, self.ticks));
            }
            DebugLoad::Memory => {
                // 2 000 pages per 2 s reading: past the 4 MiB/s line at any
                // page size the engine may be handed.
                self.swap += 2_000;
                r.pressure = Some(MemoryLevel::Critical);
                r.mem_used_pm = Some(960);
                r.mem_used_mib = Some(r.mem_mib / 100 * 96);
                r.swap_pages = Some(self.swap);
                if r.page_kib == 0 {
                    r.page_kib = 16;
                }
            }
            DebugLoad::Heat => r.thermal = Some(Thermal::Serious),
        }
    }
}

// ---- the host --------------------------------------------------------------

/// The strain engine, its probe thread and its one row.
pub(crate) struct StrainHost {
    engine: StrainTracker,
    /// `explain_heavy_load`.
    enabled: bool,
    /// The probe thread's request channel (`true` = with a sweep); `None`
    /// until the first reading an episode asks for, and again after the
    /// switch goes off.
    tx: Option<Sender<bool>>,
    /// When the in-flight request was sent.
    in_flight: Option<Instant>,
    /// The thread could not be started: never ask again this process.
    probe_failed: bool,
    /// The live strain row.
    row: Option<MessageId>,
    /// The last hardware key's dispatch.
    last_hw_key: Option<Instant>,
    /// Freezes waiting for a hardware key near their end: `(end, ms, owner)`.
    freezes: Vec<(Instant, u32, &'static str)>,
}

impl std::fmt::Debug for StrainHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrainHost")
            .field("state", &self.engine.state_word())
            .field("enabled", &self.enabled)
            .field("started", &self.tx.is_some())
            .field("in_flight", &self.in_flight.is_some())
            .finish_non_exhaustive()
    }
}

impl StrainHost {
    /// A calm host; `enabled` is `explain_heavy_load`.
    pub(crate) fn new(enabled: bool) -> Self {
        let host = Self {
            engine: StrainTracker::new(Self::config()),
            enabled,
            tx: None,
            in_flight: None,
            probe_failed: false,
            row: None,
            last_hw_key: None,
            freezes: Vec::new(),
        };
        host.publish();
        host
    }

    /// What the engine is told once: the platform's services noun
    /// (`aterm-sysprobe` owns the `cfg`), rows on the glass (ruling 208 — the
    /// records are written either way), and aterm's own pid.
    fn config() -> StrainConfig {
        StrainConfig {
            services_noun: aterm_sysprobe::SERVICES_NOUN,
            glass: true,
            self_pid: std::process::id(),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Publish the state word for `ctl metrics`.
    fn publish(&self) {
        let word = if self.enabled {
            match self.engine.state_word() {
                "suspect" => 1,
                "open" => 2,
                _ => 0,
            }
        } else {
            3
        };
        STATE.store(word, Ordering::Relaxed);
    }

    /// The engine left calm (or might have): nothing to arm while calm.
    fn calm(&self) -> bool {
        self.engine.state_word() == "calm"
    }

    /// A hardware key was dispatched at `at` (the `KeyboardInput` arm). A
    /// waiting freeze that ended near it is a hitch.
    pub(crate) fn note_hw_key(&mut self, at: Instant) {
        self.last_hw_key = Some(at);
        self.match_freezes();
    }

    /// One closed hardware key, `lag_ms` from its arrival to its window's
    /// present; the caller filtered the window and the program.
    pub(crate) fn note_key(&mut self, at: Instant, lag_ms: u32) {
        if !self.enabled {
            return;
        }
        let lag_ms = if debug_load().is_some() {
            lag_ms.max(2 * aterm_messages::SLOW_KEY_MS)
        } else {
            lag_ms
        };
        self.engine.note_key(at, lag_ms);
        self.publish();
    }

    /// Take the watchdog's freeze, if one was booked, onto the waiting list
    /// (its end moved from the metrics clock onto `now`'s).
    fn drain_freeze(&mut self, now: Instant) {
        let Some((end_ns, span_ns, owner)) = crate::watchdog::take_freeze() else {
            return;
        };
        if !self.enabled {
            return;
        }
        let age = crate::metrics::now_ns().saturating_sub(end_ns);
        let end = now.checked_sub(Duration::from_nanos(age)).unwrap_or(now);
        let ms = u32::try_from(span_ns / 1_000_000).unwrap_or(u32::MAX);
        if self.freezes.len() == PENDING_FREEZES {
            self.freezes.remove(0);
        }
        self.freezes.push((end, ms, owner.metric_name()));
        self.match_freezes();
    }

    /// Hand every waiting freeze within [`FREEZE_NEAR_KEY`] of the last
    /// hardware key to the engine — a key typed during the freeze counts
    /// (the turn that was slow may be that key's own dispatch; measured live,
    /// a starved window's 3 s turns began with the key they held) — and drop
    /// the ones no later key can reach.
    fn match_freezes(&mut self) {
        let Some(key) = self.last_hw_key else {
            return;
        };
        let mut felt = Vec::new();
        self.freezes.retain(|&(end, ms, owner)| {
            let start = end
                .checked_sub(Duration::from_millis(u64::from(ms)))
                .unwrap_or(end);
            let gap = if key >= end {
                key - end
            } else if key <= start {
                start - key
            } else {
                Duration::ZERO
            };
            if gap <= FREEZE_NEAR_KEY {
                felt.push((end, ms, owner));
                false
            } else {
                // A key already past the window means no later key can land
                // in it.
                key < end
            }
        });
        for (end, ms, owner) in felt {
            self.engine.note_freeze(end, ms, owner);
        }
        self.publish();
    }

    /// Whether a reading may be asked for at all: on, not calm, none in
    /// flight, the probe not given up on.
    fn may_read(&self) -> bool {
        self.enabled && !self.probe_failed && self.in_flight.is_none() && !self.calm()
    }

    /// The next reading's instant under `gate`, or `None` (see the module
    /// header's idle law).
    fn deadline(&self, now: Instant, gate: Gate) -> Option<Instant> {
        if !self.may_read() {
            return None;
        }
        self.engine.next_sample(now, gate)
    }

    /// Ask the probe thread for one reading (starting it on first use).
    fn request(&mut self, proxy: &EventLoopProxy<Wake>, now: Instant) {
        if self.tx.is_none() {
            match spawn_probe(proxy.clone()) {
                Ok(tx) => self.tx = Some(tx),
                Err(e) => {
                    aterm_log::warn!("strain: could not start the probe thread: {e}");
                    self.probe_failed = true;
                    return;
                }
            }
        }
        let scan = self.engine.wants_scan();
        if self.tx.as_ref().is_some_and(|tx| tx.send(scan).is_ok()) {
            self.in_flight = Some(now);
        } else {
            // The thread is gone (it ends only when its channel closes, so a
            // panic in a read): a fresh one next time.
            self.tx = None;
        }
    }

    /// A request unanswered past [`LOST_READING`] is forgotten.
    fn heal_lost(&mut self, now: Instant) {
        if self
            .in_flight
            .is_some_and(|sent| now.saturating_duration_since(sent) >= LOST_READING)
        {
            aterm_log::warn!("strain: a reading went unanswered; restarting the probe");
            self.in_flight = None;
            self.tx = None;
        }
    }

    /// The switch moved. Off parks the engine (an open row folds with its
    /// record), drops the probe thread and forgets any reading in flight.
    fn set_enabled(&mut self, on: bool, now: Instant) -> StrainOut {
        self.enabled = on;
        let out = if on {
            StrainOut::None
        } else {
            self.tx = None;
            self.in_flight = None;
            self.freezes.clear();
            self.engine.park(now)
        };
        self.publish();
        out
    }
}

/// Start the `aterm-strain-probe` thread: QoS utility, blocked on its channel
/// between requests (it never wakes itself), one `Wake::StrainReading` per
/// request. It ends when the host drops the sender.
fn spawn_probe(proxy: EventLoopProxy<Wake>) -> std::io::Result<Sender<bool>> {
    let (tx, rx) = channel::<bool>();
    std::thread::Builder::new()
        .name("aterm-strain-probe".into())
        .spawn(move || {
            // Nobody waits on a reading frame by frame, and the probe holds no
            // lock the UI thread takes: utility.
            crate::qos::set_self(crate::qos::Role::Background);
            let mut probe = aterm_sysprobe::Probe::new();
            let debug = debug_load();
            let mut fake = Saturate::default();
            while let Ok(scan) = rx.recv() {
                let t0 = Instant::now();
                let mut reading = probe.reading();
                PROBE_US_MAX.fetch_max(micros(t0.elapsed()), Ordering::Relaxed);
                let rows = scan.then(|| {
                    let t1 = Instant::now();
                    let rows = probe.scan();
                    SCAN_US_MAX.fetch_max(micros(t1.elapsed()), Ordering::Relaxed);
                    rows
                });
                if let Some(load) = debug {
                    fake.apply(load, &mut reading);
                }
                if proxy
                    .send_event(Wake::StrainReading(Box::new((reading, rows))))
                    .is_err()
                {
                    break;
                }
            }
        })?;
    Ok(tx)
}

impl App {
    /// The gate: the switch is on and some focused window's band is on screen.
    pub(crate) fn strain_gate(&self) -> Gate {
        Gate {
            enabled: self.strain.enabled(),
            focused_on_screen: self
                .windows
                .values()
                .any(|ws| ws.focused && band_on_screen(ws)),
        }
    }

    /// The loop's turn (`about_to_wait`, before the fold): take the watchdog's
    /// freeze, forget a lost reading, end an episode no reading has reached
    /// for `STALE_STRAIN` (the gate closed: it ends where sampling stopped,
    /// and the engine is calm again — riding whatever woke the loop, arming
    /// nothing), and ask for a reading that fell due.
    pub(crate) fn strain_tick(&mut self, now: Instant) {
        self.strain.drain_freeze(now);
        self.strain.heal_lost(now);
        if self.strain.enabled() && !self.strain.calm() {
            let out = self.strain.engine.lapsed(now);
            if out != StrainOut::None {
                self.perform_strain(out);
            }
        }
        if !self.strain.may_read() {
            return;
        }
        let gate = self.strain_gate();
        if self.strain.deadline(now, gate).is_some_and(|d| d <= now)
            && let Some(proxy) = self.proxy.as_ref()
        {
            self.strain.request(proxy, now);
        }
    }

    /// The next reading, folded under `DeadlineOwner::SystemStrain`.
    pub(crate) fn strain_deadline(&self, now: Instant) -> Option<Instant> {
        self.strain_deadline_if(now, self.proxy.is_some())
    }

    /// [`Self::strain_deadline`] given whether a probe can run at all (a test
    /// App has no event loop to answer it).
    pub(crate) fn strain_deadline_if(&self, now: Instant, can_probe: bool) -> Option<Instant> {
        if !can_probe || !self.strain.may_read() {
            return None;
        }
        self.strain.deadline(now, self.strain_gate())
    }

    /// One hardware key's closed, untainted input→present slice in window
    /// `wid`: a sample only when that window is focused with its band on
    /// screen and its focused session is not running a remote program.
    pub(crate) fn note_strain_key(&mut self, wid: WindowId, slice_ns: u64) {
        if !self.strain.enabled() {
            return;
        }
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if !ws.focused || !band_on_screen(ws) {
            return;
        }
        if self
            .strain_focused_program(wid)
            .is_some_and(|p| is_remote_program(&p))
        {
            return;
        }
        let ms = u32::try_from(slice_ns / 1_000_000).unwrap_or(u32::MAX);
        self.strain.note_key(Instant::now(), ms);
    }

    /// The published foreground program of `wid`'s focused session.
    fn strain_focused_program(&self, wid: WindowId) -> Option<String> {
        let session = self.session_by_id(self.focused_session_id(wid)?)?;
        let timeline = session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        timeline.agent().program.clone()
    }

    /// Every session the sweep may find a culprit under: its shell, its
    /// program, its tab, whether the person types into it, whether it is in
    /// another window.
    fn strain_sessions(&self) -> Vec<SessionRef> {
        let focused = self
            .windows
            .iter()
            .find(|(_, ws)| ws.focused)
            .map(|(wid, _)| *wid);
        let receiving = focused.and_then(|wid| self.focused_session_id(wid));
        let mut out = Vec::new();
        for (wid, ws) in &self.windows {
            for (index, tab) in ws.tab_set.tabs().iter().enumerate() {
                for view in tab.root.leaves() {
                    let Some(id) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(tab_model::View::terminal_session)
                    else {
                        continue;
                    };
                    let Some(session) = self.session_by_id(id) else {
                        continue;
                    };
                    let Ok(shell_pid) = u32::try_from(session.pid) else {
                        continue;
                    };
                    if shell_pid == 0 {
                        continue;
                    }
                    let program = session
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .agent()
                        .program
                        .clone()
                        .unwrap_or_else(|| "shell".to_string());
                    out.push(SessionRef {
                        shell_pid,
                        program,
                        tab: u16::try_from(index + 1).unwrap_or(u16::MAX),
                        receiving_keys: receiving == Some(id),
                        elsewhere: focused != Some(*wid),
                    });
                }
            }
        }
        out
    }

    /// The probe answered: sweep first (the engine groups it), then the
    /// reading; perform what the engine says.
    pub(crate) fn on_strain_reading(&mut self, reading: Reading, rows: Option<Vec<ProcRow>>) {
        self.strain.in_flight = None;
        if !self.strain.enabled() {
            return;
        }
        if let Some(rows) = rows {
            let sessions = self.strain_sessions();
            // aterm's own jobs are aterm's children: the sweep's ppid chain
            // reaches aterm and the engine records them as `aterm itself`,
            // never names them.
            self.strain
                .engine
                .scanned(reading.at, &rows, &sessions, &[]);
        }
        let gate = self.strain_gate();
        let out = self.strain.engine.observe(&reading, gate);
        self.perform_strain(out);
    }

    /// The kernel pushed Critical memory pressure.
    pub(crate) fn strain_memory_critical(&mut self) {
        if !self.strain.enabled() {
            return;
        }
        let out = self.strain.engine.pushed_critical(Instant::now());
        self.perform_strain(out);
    }

    /// `explain_heavy_load` may have moved (a config reload).
    pub(crate) fn reconfigure_strain(&mut self) {
        let on = self.config.explain_heavy_load_or_default();
        if on == self.strain.enabled() {
            return;
        }
        let out = self.strain.set_enabled(on, Instant::now());
        self.perform_strain(out);
        if !on && let Some(id) = self.strain.row.take() {
            // Whatever the engine folded, the switch off leaves no row.
            self.withdraw_message(id);
        }
    }

    /// Do what the engine said. A fold withdraws the row (a Vanish, never ✓)
    /// and its log entry BECOMES the episode's record (`withdraw_as`: one
    /// entry per episode on Settings ▸ Messages, not the withdrawn row beside
    /// a record repeating it). A record with no live row to become (the row
    /// faded, or never showed) is posted as its own; never under a live row's
    /// key, which it would supersede silently.
    fn perform_strain(&mut self, out: StrainOut) {
        match out {
            StrainOut::None => {}
            StrainOut::Post(msg) => {
                let id = self.post_message(msg);
                self.strain.row = Some(id);
            }
            StrainOut::Restate(msg) => {
                let restated = self
                    .strain
                    .row
                    .is_some_and(|id| self.restate_message(id, restatement_of(&msg)));
                if !restated {
                    // The row faded (its staleness cap) under a keepalive:
                    // back on the glass with the same words.
                    let id = self.post_message(msg);
                    self.strain.row = Some(id);
                }
            }
            StrainOut::Fold { record } => {
                let row = self.strain.row.take();
                let became = match (row, record.as_ref()) {
                    (Some(id), Some(rec)) => {
                        let done =
                            self.messages
                                .withdraw_as(id, &rec.title, &rec.detail, Instant::now());
                        if done {
                            self.sync_messages();
                        }
                        done
                    }
                    _ => false,
                };
                if !became {
                    if let Some(id) = row {
                        self.withdraw_message(id);
                    }
                    if let Some(record) = record {
                        self.record_message(record);
                    }
                }
            }
            StrainOut::Record(msg) => {
                self.record_message(msg);
            }
        }
        self.strain.publish();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use aterm_messages::STRAIN_KEY;
    use aterm_messages::strain::Reading;

    use super::REMOTE_PROGRAMS;
    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId};

    const WID: WindowId = WindowId(0);

    /// A headless App whose first window is focused and ON SCREEN (the test
    /// seam for a real, unoccluded OS window).
    fn on_screen_app() -> App {
        let mut app = App::headless_for_test();
        let ws = app.windows.get_mut(&WID).expect("window 0");
        ws.band_on_screen_for_test = true;
        ws.focused = true;
        app
    }

    /// Publish `program` as the focused session's foreground program.
    fn set_program(app: &App, program: &str) {
        let id = app.focused_session_id(WID).expect("a focused session");
        let session = app.session_by_id(id).expect("the session");
        let mut timeline = session.ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(4242);
        timeline.set_program(4242, Some(program.to_string()));
    }

    /// Twelve hardware keys, each 120 ms from arrival to its echo — FELT.
    fn slow_keys(app: &mut App) {
        for _ in 0..12 {
            app.note_strain_key(WID, 120_000_000);
        }
    }

    /// Only a HARDWARE key's slice is a sample (ruling 206): a control key
    /// through the same input seam arms a stamp that is not one, and a key
    /// typed into a remote program (ssh, mosh, et, telnet, kubectl, docker)
    /// is dropped by the host, however slow — a slow network is not a slow
    /// Mac. The same keys into a local program open a Suspect.
    #[test]
    fn ctl_send_and_remote_sessions_are_not_samples() {
        let mut app = on_screen_app();
        let _ = app.input(
            WID,
            InputEvent::Text("a".into()),
            Source::Controller {
                op: aterm_session::Op::WriteInput,
            },
        );
        let pending = app.windows[&WID].pending_input;
        assert!(pending.is_pending(), "the control key armed the stamp");
        assert!(
            !pending.armed_by_hardware(),
            "a ctl send/feed/key is never a strain sample"
        );
        // The `KeyboardInput` arm brackets its dispatch with the flag.
        app.windows.get_mut(&WID).unwrap().pending_input = Default::default();
        app.hw_key_dispatch = true;
        let _ = app.input(WID, InputEvent::Text("b".into()), Source::Human);
        app.hw_key_dispatch = false;
        assert!(
            app.windows[&WID].pending_input.armed_by_hardware(),
            "the hardware arm's key is"
        );

        for remote in REMOTE_PROGRAMS {
            let mut app = on_screen_app();
            set_program(&app, remote);
            slow_keys(&mut app);
            assert_eq!(
                app.strain.engine.state_word(),
                "calm",
                "{remote}: a remote program's keys are no samples"
            );
        }
        let mut app = on_screen_app();
        set_program(&app, "zsh");
        slow_keys(&mut app);
        assert_eq!(
            app.strain.engine.state_word(),
            "suspect",
            "a local one's are"
        );
    }

    /// THE IDLE LAW at the host: calm arms nothing; a Suspect samples only
    /// while a focused window's band is on screen — occluded (minimized,
    /// covered), headless and unfocused windows arm nothing — and nothing is
    /// armed while a reading is in flight or where no probe can run.
    #[test]
    fn an_occluded_window_arms_no_system_strain() {
        let mut app = on_screen_app();
        let now = Instant::now();
        assert_eq!(app.strain_deadline_if(now, true), None, "calm");
        slow_keys(&mut app);
        assert_eq!(app.strain.engine.state_word(), "suspect");
        assert!(
            app.strain_deadline_if(now, true).is_some(),
            "a suspect, focused, on-screen window samples"
        );

        app.windows.get_mut(&WID).unwrap().occluded = true;
        assert_eq!(app.strain_deadline_if(now, true), None, "occluded");
        app.windows.get_mut(&WID).unwrap().occluded = false;

        app.windows.get_mut(&WID).unwrap().band_on_screen_for_test = false;
        assert_eq!(app.strain_deadline_if(now, true), None, "headless");
        app.windows.get_mut(&WID).unwrap().band_on_screen_for_test = true;

        app.windows.get_mut(&WID).unwrap().focused = false;
        assert_eq!(app.strain_deadline_if(now, true), None, "unfocused");
        // …and an unfocused window's keys are no samples either.
        let before = app.strain.engine.clone();
        slow_keys(&mut app);
        assert_eq!(
            format!("{:?}", app.strain.engine),
            format!("{before:?}"),
            "an unfocused window's slices never reach the engine"
        );
        app.windows.get_mut(&WID).unwrap().focused = true;

        app.strain.in_flight = Some(now);
        assert_eq!(app.strain_deadline_if(now, true), None, "in flight");
        app.strain.in_flight = None;

        assert_eq!(
            app.strain_deadline(now),
            None,
            "a test App has no event loop: no probe, no deadline"
        );
    }

    /// A reading of a machine `busy_pm` busy on 8 cores, cumulative over `n`
    /// readings.
    fn busy(at: Instant, n: u64, busy_pm: u64) -> Reading {
        let mut r = Reading::empty(at);
        r.cores = 8;
        r.mem_mib = 16_384;
        r.busy_ticks = Some((n * busy_pm * 8, n * 8_000));
        r
    }

    /// Drive an episode to an open row: slow keys every 300 ms and a
    /// 95 %-busy reading every 2 s for `secs` seconds from `t0`.
    fn drive(app: &mut App, t0: Instant, secs: u64) {
        let mut n = 0;
        for step in 0..=(secs * 10) {
            let at = t0 + Duration::from_millis(step * 100);
            if step % 3 == 0 {
                app.strain.note_key(at, 120);
            }
            if step > 0 && step % 20 == 0 {
                n += 1;
                app.on_strain_reading(busy(at, n, 950), None);
            }
        }
    }

    /// `explain_heavy_load = false` (a live reload) folds an open row — the
    /// row withdraws — parks the engine, drops the probe, and arms nothing
    /// after: later keys are no samples. On again, the next felt keys start
    /// a fresh episode.
    #[test]
    fn the_switch_off_arms_nothing() {
        let mut app = on_screen_app();
        let t0 = Instant::now();
        drive(&mut app, t0, 12);
        assert_eq!(app.strain.engine.state_word(), "open");
        let row = app
            .messages
            .live_by_key(STRAIN_KEY)
            .expect("the strain row is on the glass");
        assert!(
            row.msg.title.starts_with("Typing slowed by"),
            "{}",
            row.msg.title
        );
        assert_eq!(app.strain.row, Some(row.id));

        app.config.explain_heavy_load = Some(false);
        app.reconfigure_strain();
        assert!(!app.strain.enabled());
        assert!(
            app.messages.live_by_key(STRAIN_KEY).is_none(),
            "the row withdrew"
        );
        assert_eq!(app.strain.row, None);
        assert_eq!(app.strain.engine.state_word(), "calm", "parked");
        assert!(app.strain.tx.is_none(), "the probe thread is dropped");
        let later = t0 + Duration::from_secs(13);
        assert_eq!(app.strain_deadline_if(later, true), None);
        slow_keys(&mut app);
        assert_eq!(
            app.strain.engine.state_word(),
            "calm",
            "keys are no samples"
        );
        assert_eq!(app.strain_deadline_if(later, true), None);
        // A reading already in flight when the switch went off is dropped.
        app.on_strain_reading(busy(later, 7, 950), None);
        assert!(app.messages.live_by_key(STRAIN_KEY).is_none());

        app.config.explain_heavy_load = Some(true);
        app.reconfigure_strain();
        slow_keys(&mut app);
        assert_eq!(app.strain.engine.state_word(), "suspect", "on again");
    }

    /// A fold leaves nothing armed: once the load goes, the row withdraws
    /// (a Vanish) and the host arms no further reading.
    #[test]
    fn a_fold_withdraws_the_row_and_arms_nothing() {
        let mut app = on_screen_app();
        let t0 = Instant::now();
        drive(&mut app, t0, 12);
        assert!(app.messages.live_by_key(STRAIN_KEY).is_some());
        // The load ends: idle readings (keys still typed, still slow).
        let mut n = 6;
        let mut at = t0 + Duration::from_secs(12);
        for _ in 0..8 {
            at += Duration::from_secs(2);
            n += 1;
            app.strain.note_key(at, 120);
            // Cumulative ticks at 5 % from here on.
            let mut r = busy(at, n, 950);
            r.busy_ticks = Some((6 * 950 * 8 + (n - 6) * 50 * 8, n * 8_000));
            app.on_strain_reading(r, None);
        }
        assert!(
            app.messages.live_by_key(STRAIN_KEY).is_none(),
            "the row folded"
        );
        assert_eq!(app.strain.row, None);
        assert_ne!(app.strain.engine.state_word(), "open");
        // ONE log entry for the episode: the withdrawn row became its record
        // (never the row beside a record repeating it).
        let entries: Vec<_> = strain_entries(&app);
        assert_eq!(entries.len(), 1, "{entries:?}");
        let (title, detail, how) = &entries[0];
        assert!(
            title.starts_with("Typing slowed by ") && title.contains(" for "),
            "{title}"
        );
        assert!(
            detail.iter().any(|l| l.starts_with("on glass ")),
            "{detail:?}"
        );
        assert_eq!(how.as_deref(), Some("withdrawn"));
    }

    /// Every `system.strain` log entry: title, detail, how it retired.
    fn strain_entries(app: &App) -> Vec<(String, Vec<String>, Option<&'static str>)> {
        app.messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(STRAIN_KEY))
            .map(|r| {
                (
                    r.title.clone(),
                    r.detail.clone(),
                    r.retired().map(|how| how.as_word()),
                )
            })
            .collect()
    }

    /// The window loses focus with a row open: nothing is armed (the idle
    /// law), and once no reading has come for `STALE_STRAIN` the loop's next
    /// turn — whatever woke it — ends the episode WHERE SAMPLING STOPPED: the
    /// engine is calm, the row is gone, and the record never counts the time
    /// away as time on the glass.
    #[test]
    fn an_unfocused_episode_ends_where_sampling_stopped() {
        let mut app = on_screen_app();
        let t0 = Instant::now();
        drive(&mut app, t0, 12);
        assert_eq!(app.strain.engine.state_word(), "open");
        app.windows.get_mut(&WID).unwrap().focused = false;
        let away = t0 + Duration::from_secs(12 + 3600);
        assert_eq!(app.strain_deadline_if(away, true), None, "nothing armed");
        app.strain_tick(t0 + Duration::from_secs(20));
        assert_eq!(app.strain.engine.state_word(), "open", "not stale yet");
        app.strain_tick(away);
        assert_eq!(app.strain.engine.state_word(), "calm");
        assert!(app.messages.live_by_key(STRAIN_KEY).is_none());
        let entries = strain_entries(&app);
        assert_eq!(entries.len(), 1, "{entries:?}");
        let (title, detail, _) = &entries[0];
        assert!(title.ends_with(" s"), "seconds, not the hour away: {title}");
        let glass = detail
            .iter()
            .find(|l| l.starts_with("on glass "))
            .expect("the on-glass line");
        assert!(glass.ends_with(" s"), "seconds, not the hour away: {glass}");
    }

    /// Only the six remote programs are exempt, by exact name.
    #[test]
    fn remote_programs_are_named_exactly() {
        for p in REMOTE_PROGRAMS {
            assert!(super::is_remote_program(p), "{p}");
        }
        for p in ["zsh", "cargo", "sshd", "yes", "dockerd", "et-cetera", ""] {
            assert!(!super::is_remote_program(p), "{p}");
        }
    }

    /// A freeze near a hardware key is a hitch; one far from any key is not.
    #[test]
    fn a_freeze_counts_only_near_a_hardware_key() {
        let mut host = super::StrainHost::new(true);
        let t0 = Instant::now();
        // Two freezes with no key near them: nothing felt.
        host.freezes.push((t0, 600, "user_event"));
        host.note_hw_key(t0 + Duration::from_secs(2));
        assert!(host.freezes.is_empty(), "a key past the window drops it");
        assert_eq!(host.engine.state_word(), "calm");
        // Two freezes each right before a key: two hitches, FELT.
        for k in 0..2u64 {
            let end = t0 + Duration::from_secs(3 + k);
            host.freezes.push((end, 600, "window_event"));
            host.note_hw_key(end + Duration::from_millis(40));
        }
        assert_eq!(host.engine.state_word(), "suspect");

        // A key dispatched INSIDE a long turn (the turn is that key's own
        // slow dispatch): the freeze ends 3 s after the key, and still
        // counts. Two of them are FELT.
        let mut host = super::StrainHost::new(true);
        for k in 0..2u64 {
            let key = t0 + Duration::from_secs(1 + 4 * k);
            host.note_hw_key(key);
            host.freezes
                .push((key + Duration::from_secs(3), 3_200, "window_event"));
            host.match_freezes();
        }
        assert!(host.freezes.is_empty(), "both matched");
        assert_eq!(host.engine.state_word(), "suspect");
    }
}
