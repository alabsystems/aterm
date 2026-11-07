// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats deck's strip checkpoint, append-only (`docs/DESIGN-stats-deck-flow-2026-11-02.md`
//! §9): a folder in the state home's quiet `.ubd` folder, so its churn feeds neither fseventsd
//! nor Spotlight, holding `base.json`, the whole strip, and `tail.jsonl`, one line per closed
//! ten-second epoch of every ring. The base is rewritten at a start's first reading, hourly, where
//! the model's held buckets changed other than by an epoch closing
//! ([`aterm_vitals::Vitals::strip_rewrites`]) and at the probe thread's stop; between bases, and
//! while an hourly one cannot be written, each reading past an epoch's end appends that epoch's
//! line. A restore reads the base and replays the tail lines of its generation past its last
//! epoch ([`SavedStrip::replay`]), stopping at the first line that does not parse, so a crash
//! between a base's rename and the tail's truncation counts nothing twice.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_vitals::model::{STRIP_RINGS, SavedStrip, StripEvent, StripEventKind, StripLine};
use aterm_vitals::ring::{Agg, GapKind, SavedBucket, SavedRing, TimeGap};

use super::{StripLock, read_bounded};

/// The strip's folder, under the state home's [`aterm_tempfile::QUIET`] folder.
pub(super) const STRIP_DIR: &str = "vitals-strip.noindex";
/// The whole strip, rewritten atomically.
pub(super) const BASE_FILE: &str = "base.json";
/// The closed epochs since the base, one line each, appended.
pub(super) const TAIL_FILE: &str = "tail.jsonl";
/// Where a build before the append-only checkpoint kept the strip, whole, at schema 1: it restores
/// nothing (record §9) and goes at the first base written.
pub(super) const LEGACY_FILE: &str = "vitals-strip.json";
/// Schema 2: time-weighted buckets, seven hours of ten-minute ones (`LONG_BUCKETS` 43), and a
/// Processes ring that holds starts where a start counter reads.
pub(super) const STRIP_SCHEMA: u32 = 2;
/// The bound a base is read under; a full one is held under 85 % of it by a test.
pub(super) const STRIP_MAX_BYTES: u64 = 1024 * 1024;
/// The bound a tail is read under: four hours of lines at their fullest. A base is written
/// hourly, so only an hourly base that cannot be written lets a tail grow past an hour's, and an
/// append never takes it past this bound ([`StripStore::keep`]).
pub(super) const TAIL_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// How often a base is rewritten while nothing else asks for one.
pub(super) const BASE_EVERY: Duration = Duration::from_secs(3_600);
/// How soon a base that could not be written is tried again.
const RETRY_EVERY: Duration = Duration::from_secs(10);
/// The most epochs one reading appends: an hour's, the ring's whole span.
const LINES_AT_ONCE: i64 = 360;

/// The strip's folder in the state folder `home`.
pub(super) fn strip_dir(home: &Path) -> PathBuf {
    home.join(aterm_tempfile::QUIET).join(STRIP_DIR)
}

/// The strip checkpoint a probe thread keeps.
pub(super) struct StripStore {
    /// The folder; `None` with no state home, which keeps nothing.
    dir: Option<PathBuf>,
    /// The schema-1 file to remove at the first base written, while it may be there.
    legacy: Option<PathBuf>,
    /// The generation of the base on disk: its tail lines carry it.
    generation: u64,
    /// The newest epoch the files hold closed: the next line is past it.
    through_k: i64,
    /// When the base was last written; `None` while one is due at the next reading
    /// ([`Self::arm`]).
    base_at: Option<Instant>,
    /// When a base last failed to be written, for [`RETRY_EVERY`].
    failed_at: Option<Instant>,
    /// The model's [`aterm_vitals::Vitals::strip_rewrites`] the base on disk was written at.
    rewrites: u64,
    /// The gaps the files hold: a line carries those not among them.
    gaps: Vec<TimeGap>,
    /// The events the files hold.
    events: Vec<StripEvent>,
    /// The tail's length in whole lines of this generation.
    tail_ok: u64,
    /// Whether the tail may hold more than [`Self::tail_ok`] (a short or failed append, a
    /// truncation that failed): the next append truncates it back first.
    tail_dirty: bool,
    /// Bytes written to the base and the tail.
    #[cfg(test)]
    written: u64,
}

impl StripStore {
    /// The checkpoint in `dir`, and the strip it holds: its base with its tail replayed. `legacy`
    /// names the schema-1 file, which restores nothing. A base is due at the first reading.
    pub(super) fn open(dir: Option<PathBuf>, legacy: Option<PathBuf>) -> (Self, SavedStrip) {
        let (generation, kept) = dir.as_deref().map(restored).unwrap_or_default();
        let store = Self {
            dir,
            legacy,
            generation,
            through_k: kept.through_k,
            base_at: None,
            failed_at: None,
            rewrites: 0,
            gaps: kept.time_gaps.clone(),
            events: kept.strip_events.clone(),
            tail_ok: 0,
            tail_dirty: true,
            #[cfg(test)]
            written: 0,
        };
        (store, kept)
    }

    /// When the base was last written, unix ms, by its file: a clean stop's write takes no
    /// reading of its own (`vitals_host::open_strip`).
    pub(super) fn written_ms(&self) -> Option<i64> {
        crate::vitals_startup::written_ms(&self.dir.as_deref()?.join(BASE_FILE))
    }

    /// Makes a base due at the next reading.
    pub(super) const fn arm(&mut self) {
        self.base_at = None;
    }

    /// The checkpoint after a reading of `model` at `now`: a base where one is due, else the
    /// lines of the epochs closed since the files' newest. While a due base cannot be written,
    /// lines go on onto the base on disk where they still hold ([`Self::tail_holds`]): an hourly
    /// base that fails (a full disk refusing its few hundred KB) loses no closed epoch while the
    /// tail stays under [`TAIL_MAX_BYTES`]. Neither is written while a live peer holds `lock`;
    /// they stay due.
    pub(super) fn keep(
        &mut self,
        model: &aterm_vitals::Vitals,
        now: Instant,
        lock: &mut StripLock,
    ) {
        if self.dir.is_none() {
            return;
        }
        if self.base_due(model, now) {
            if self
                .failed_at
                .is_none_or(|at| now.saturating_duration_since(at) >= RETRY_EVERY)
                && lock.writes()
                && self.put_base(&model.saved_strip(false), model.strip_rewrites(), now)
            {
                return;
            }
            if !self.tail_holds(model) {
                return;
            }
        }
        let Some(open) = model.strip_open_k() else {
            return;
        };
        if open.saturating_sub(1) <= self.through_k || !lock.writes() {
            return;
        }
        self.append(model, open);
    }

    /// The stop's write: the whole strip, clean where the app quit.
    pub(super) fn stop(
        &mut self,
        model: &aterm_vitals::Vitals,
        quit: bool,
        now: Instant,
        lock: &mut StripLock,
    ) {
        if self.dir.is_some() && lock.writes() {
            self.put_base(&model.saved_strip(quit), model.strip_rewrites(), now);
        }
    }

    fn base_due(&self, model: &aterm_vitals::Vitals, now: Instant) -> bool {
        model.strip_rewrites() != self.rewrites
            || self
                .base_at
                .is_none_or(|at| now.saturating_duration_since(at) >= BASE_EVERY)
    }

    /// Whether `model`'s closed epochs still go onto the base on disk as lines: this run wrote
    /// that base, and the model's held buckets have changed only by epochs closing since. A base
    /// due for any other reason (a start's first, a restore, a backward clock step's re-key, a
    /// Processes row whose meaning moved) stops the tail until it is written, since lines on the
    /// old base would mix what the model no longer holds with what it does.
    fn tail_holds(&self, model: &aterm_vitals::Vitals) -> bool {
        self.base_at.is_some() && model.strip_rewrites() == self.rewrites
    }

    /// Writes `saved` as the base, of the next generation, then empties the tail; `rewrites`
    /// is the model's count it was taken at. A strip that has read nothing is not written.
    /// Whether it was written.
    pub(super) fn put_base(&mut self, saved: &SavedStrip, rewrites: u64, now: Instant) -> bool {
        let Some(dir) = self.dir.clone() else {
            return false;
        };
        if saved.last_wall_ms <= 0 {
            return false;
        }
        let generation = self.generation.wrapping_add(1);
        let Some(text) = base_json(saved, generation) else {
            return false;
        };
        let written =
            aterm_types::fs_restricted::create_dir_all_unshared(&dir, 0o700).and_then(|()| {
                aterm_agent::harness::upgrade_models::write_atomic(&dir.join(BASE_FILE), &text)
            });
        if let Err(error) = written {
            aterm_log::warn!(
                "system vitals: the strip's base could not be written to {}: {error}",
                dir.display()
            );
            self.failed_at = Some(now);
            return false;
        }
        #[cfg(test)]
        {
            self.written += text.len() as u64;
        }
        self.generation = generation;
        self.through_k = saved.through_k;
        self.base_at = Some(now);
        self.failed_at = None;
        self.rewrites = rewrites;
        self.gaps.clone_from(&saved.time_gaps);
        self.events.clone_from(&saved.strip_events);
        self.tail_ok = 0;
        self.tail_dirty = !truncate(&dir.join(TAIL_FILE), 0);
        if let Some(legacy) = self.legacy.take()
            && let Err(error) = std::fs::remove_file(&legacy)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            aterm_log::warn!(
                "system vitals: the schema-1 strip {} could not be removed: {error}",
                legacy.display()
            );
        }
        true
    }

    /// Appends the lines of the epochs `model` closed past [`Self::through_k`], before `open`,
    /// the last carrying the gaps and events the files do not hold yet.
    fn append(&mut self, model: &aterm_vitals::Vitals, open: i64) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let from = self
            .through_k
            .saturating_add(1)
            .max(open.saturating_sub(LINES_AT_ONCE));
        let mut lines: Vec<StripLine> = (from..open).filter_map(|k| model.strip_line(k)).collect();
        let Some(last) = lines.last_mut() else {
            self.through_k = open - 1;
            return;
        };
        let (gaps, events) = model.strip_marks();
        last.time_gaps = gaps
            .iter()
            .filter(|g| !self.gaps.contains(g))
            .copied()
            .collect();
        last.strip_events = events
            .iter()
            .filter(|e| !self.events.contains(e))
            .cloned()
            .collect();
        let text: String = lines
            .iter()
            .filter_map(|line| line_json(line, self.generation))
            .map(|json| json + "\n")
            .collect();
        if self.tail_ok.saturating_add(text.len() as u64) > TAIL_MAX_BYTES {
            // Past its bound a tail restores nothing, so these epochs wait for the next base.
            return;
        }
        let path = dir.join(TAIL_FILE);
        if self.tail_dirty && !truncate(&path, self.tail_ok) {
            return;
        }
        match append_to(&path, text.as_bytes()) {
            Ok(()) => {
                #[cfg(test)]
                {
                    self.written += text.len() as u64;
                }
                self.tail_ok += text.len() as u64;
                self.tail_dirty = false;
                self.through_k = open - 1;
                self.gaps = gaps;
                self.events = events;
            }
            Err(error) => {
                self.tail_dirty = true;
                aterm_log::warn!(
                    "system vitals: the strip's tail could not be appended to {}: {error}",
                    path.display()
                );
            }
        }
    }

    /// Bytes written to the base and the tail since the open.
    #[cfg(test)]
    pub(super) const fn bytes_written(&self) -> u64 {
        self.written
    }
}

/// Cuts the file at `path` to `len` bytes; a missing file is already empty. Whether it holds no
/// more than that now.
fn truncate(path: &Path, len: u64) -> bool {
    let mut open = std::fs::OpenOptions::new();
    open.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    match open.open(path).and_then(|file| file.set_len(len)) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            aterm_log::warn!(
                "system vitals: the strip's tail {} could not be cut back: {error}",
                path.display()
            );
            false
        }
    }
}

/// `bytes` appended to the regular file at `path`, created 0600 where missing, never through a
/// link.
fn append_to(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut open = std::fs::OpenOptions::new();
    open.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = open.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the tail is not a regular file",
        ));
    }
    file.write_all(bytes)
}

/// The strip `dir` holds and its base's generation: the base with each tail line of that
/// generation replayed, up to the first line that does not parse. No base (a folder a schema-1
/// build never made, a damaged base, one of another schema) holds nothing.
fn restored(dir: &Path) -> (u64, SavedStrip) {
    let Some((generation, mut kept)) =
        read_bounded(&dir.join(BASE_FILE), STRIP_MAX_BYTES).and_then(|text| base_of(&text))
    else {
        return (0, SavedStrip::default());
    };
    let tail = read_bounded(&dir.join(TAIL_FILE), TAIL_MAX_BYTES).unwrap_or_default();
    for line in whole_lines(&tail) {
        let Some((of, line)) = line_of(line) else {
            break;
        };
        if of == generation {
            kept.replay(line);
        }
    }
    (generation, kept)
}

/// The newline-ended lines of `text`, in order: a last line with no newline was cut short.
fn whole_lines(text: &str) -> impl Iterator<Item = &str> {
    let whole = text.rfind('\n').map_or("", |end| &text[..end]);
    whole.split('\n').filter(|_| !whole.is_empty())
}

/// One kept bucket: `(key, τ ms, n, over ms, mean, min, max)`.
type AggDoc = (i64, u32, u16, u32, f64, f64, f64);

pub(super) fn agg_doc(&(k, a): &SavedBucket) -> AggDoc {
    (k, a.tau_ms, a.n, a.over_ms, a.mean, a.min, a.max)
}

pub(super) fn agg_of((k, tau_ms, n, over_ms, mean, min, max): AggDoc) -> SavedBucket {
    (
        k,
        Agg {
            tau_ms,
            n,
            over_ms,
            mean,
            min,
            max,
        },
    )
}

const fn unfolded() -> i64 {
    i64::MIN
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_unfolded(k: &i64) -> bool {
    *k == i64::MIN
}

/// One ring of a base: its 10 s buckets, its raw readings, its ten-minute buckets and where its
/// folding into them stands ([`SavedRing`]).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct RingDoc {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    b: Vec<AggDoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    r: Vec<(i64, f64, bool, u32)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    l: Vec<AggDoc>,
    #[serde(default = "unfolded", skip_serializing_if = "is_unfolded")]
    f: i64,
}

impl RingDoc {
    fn of(r: &SavedRing) -> Self {
        Self {
            b: r.buckets.iter().map(agg_doc).collect(),
            r: r.recent.clone(),
            l: r.long.iter().map(agg_doc).collect(),
            f: r.folded_below,
        }
    }

    fn into_saved(self) -> SavedRing {
        SavedRing {
            buckets: self.b.into_iter().map(agg_of).collect(),
            recent: self.r,
            long: self.l.into_iter().map(agg_of).collect(),
            folded_below: self.f,
        }
    }
}

/// `base.json`: the whole strip at [`STRIP_SCHEMA`], its rings by slot
/// ([`SavedStrip::ring_mut`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BaseDoc {
    schema: u32,
    generation: u64,
    last_wall_ms: i64,
    through_k: i64,
    clean_stop: bool,
    caps: Vec<Option<f64>>,
    rings: Vec<RingDoc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proc_starts: Option<bool>,
    #[serde(default)]
    gaps: Vec<GapDoc>,
    #[serde(default)]
    events: Vec<EventDoc>,
}

/// A gap: `(start ms, end ms, kind)`.
type GapDoc = (i64, i64, String);
/// An event: `(at ms, kind, label)`.
type EventDoc = (i64, String, String);

/// One line of `tail.jsonl`: generation `g`'s epoch `k`, its newest wall clock `w`, the
/// capacities `c`, each ring's bucket by slot `r`, what the Processes rings hold `p`, and the
/// gaps and events opened since the line before.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LineDoc {
    g: u64,
    k: i64,
    w: i64,
    c: Vec<Option<f64>>,
    r: Vec<(u8, u32, u16, u32, f64, f64, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    p: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    gaps: Vec<GapDoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events: Vec<EventDoc>,
}

fn caps_of(read: Vec<Option<f64>>) -> [Option<f64>; 7] {
    let mut caps = [None; 7];
    for (dst, src) in caps.iter_mut().zip(read) {
        *dst = src.filter(|x| x.is_finite() && *x > 0.0);
    }
    caps
}

fn gap_doc(g: &TimeGap) -> GapDoc {
    (g.start_ms, g.end_ms, gap_kind_word(g.kind).to_owned())
}

fn gap_of((start_ms, end_ms, kind): GapDoc) -> Option<TimeGap> {
    (end_ms > start_ms).then_some(TimeGap {
        start_ms,
        end_ms,
        kind: gap_kind_of(&kind)?,
    })
}

fn event_doc(e: &StripEvent) -> EventDoc {
    (
        e.at_unix_ms,
        strip_event_kind_word(e.kind).to_owned(),
        e.label.clone(),
    )
}

fn event_of((at_unix_ms, kind, label): EventDoc) -> Option<StripEvent> {
    Some(StripEvent {
        at_unix_ms,
        kind: strip_event_kind_of(&kind)?,
        label,
    })
}

/// `strip` as a base of `generation`; `None` for one that has read nothing.
pub(super) fn base_json(strip: &SavedStrip, generation: u64) -> Option<String> {
    if strip.last_wall_ms <= 0 {
        return None;
    }
    let doc = BaseDoc {
        schema: STRIP_SCHEMA,
        generation,
        last_wall_ms: strip.last_wall_ms,
        through_k: strip.through_k,
        clean_stop: strip.clean_stop,
        caps: strip.caps.to_vec(),
        rings: (0..STRIP_RINGS)
            .map(|slot| strip.ring(slot).map(RingDoc::of).unwrap_or_default())
            .collect(),
        proc_starts: strip.proc_starts,
        gaps: strip.time_gaps.iter().map(gap_doc).collect(),
        events: strip.strip_events.iter().map(event_doc).collect(),
    };
    aterm_json::to_string(&doc).ok()
}

/// The base in `text` and its generation; `None` unless it parses at [`STRIP_SCHEMA`] with a
/// reading.
pub(super) fn base_of(text: &str) -> Option<(u64, SavedStrip)> {
    let doc: BaseDoc = aterm_json::from_str(text).ok()?;
    if doc.schema != STRIP_SCHEMA || doc.last_wall_ms <= 0 {
        return None;
    }
    let mut strip = SavedStrip {
        last_wall_ms: doc.last_wall_ms,
        through_k: doc.through_k,
        clean_stop: doc.clean_stop,
        caps: caps_of(doc.caps),
        proc_starts: doc.proc_starts,
        time_gaps: doc.gaps.into_iter().filter_map(gap_of).collect(),
        strip_events: doc.events.into_iter().filter_map(event_of).collect(),
        ..SavedStrip::default()
    };
    for (slot, ring) in doc.rings.into_iter().enumerate() {
        if let Some(dst) = strip.ring_mut(slot) {
            *dst = ring.into_saved();
        }
    }
    Some((doc.generation, strip))
}

/// `line` as a tail line of `generation`.
pub(super) fn line_json(line: &StripLine, generation: u64) -> Option<String> {
    let doc = LineDoc {
        g: generation,
        k: line.k,
        w: line.wall_ms,
        c: line.caps.to_vec(),
        r: line
            .rings
            .iter()
            .map(|&(slot, a)| (slot, a.tau_ms, a.n, a.over_ms, a.mean, a.min, a.max))
            .collect(),
        p: line.proc_starts,
        gaps: line.time_gaps.iter().map(gap_doc).collect(),
        events: line.strip_events.iter().map(event_doc).collect(),
    };
    aterm_json::to_string(&doc).ok()
}

/// The tail line in `text` and its generation; `None` where it does not parse.
pub(super) fn line_of(text: &str) -> Option<(u64, StripLine)> {
    let doc: LineDoc = aterm_json::from_str(text).ok()?;
    let line = StripLine {
        k: doc.k,
        wall_ms: doc.w,
        caps: caps_of(doc.c),
        rings: doc
            .r
            .into_iter()
            .map(|(slot, tau_ms, n, over_ms, mean, min, max)| {
                (
                    slot,
                    Agg {
                        tau_ms,
                        n,
                        over_ms,
                        mean,
                        min,
                        max,
                    },
                )
            })
            .collect(),
        proc_starts: doc.p,
        time_gaps: doc.gaps.into_iter().filter_map(gap_of).collect(),
        strip_events: doc.events.into_iter().filter_map(event_of).collect(),
    };
    Some((doc.g, line))
}

fn gap_kind_word(kind: GapKind) -> &'static str {
    match kind {
        GapKind::Paused => "paused",
        GapKind::Sleep => "sleep",
        GapKind::Crash => "crash",
    }
}

fn gap_kind_of(word: &str) -> Option<GapKind> {
    match word {
        "paused" => Some(GapKind::Paused),
        "sleep" => Some(GapKind::Sleep),
        "crash" => Some(GapKind::Crash),
        _ => None,
    }
}

fn strip_event_kind_word(kind: StripEventKind) -> &'static str {
    match kind {
        StripEventKind::SleepWake => "sleep_wake",
        StripEventKind::PressureSpike => "pressure_spike",
        StripEventKind::Restart => "restart",
        StripEventKind::Crash => "crash",
    }
}

fn strip_event_kind_of(word: &str) -> Option<StripEventKind> {
    match word {
        "sleep_wake" => Some(StripEventKind::SleepWake),
        "pressure_spike" => Some(StripEventKind::PressureSpike),
        "restart" => Some(StripEventKind::Restart),
        "crash" => Some(StripEventKind::Crash),
        _ => None,
    }
}

#[cfg(test)]
#[path = "vitals_strip_tests.rs"]
mod tests;
