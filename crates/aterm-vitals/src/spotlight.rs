// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spotlight read whole (SD-93): the family's cost over a split window, summed from its
//! Apple-signed coalitions ([`reading`]), and each local volume's indexing state from
//! `mdutil -s -a`, which needs no root ([`volumes`]), run at most once a minute ([`Cadence`])
//! over a port with an inert test arm ([`MdutilProbe`]).
//!
//! The family's figures come from the coalition rows of the split, never from the process
//! sweep: the system-owned `mds` (root) and `mds_stores` (`_mds_stores`) read `refused` there.
//! So a reading exists only on a split tick, and a family process inside another lead's
//! coalition is not counted.

use crate::explain::{CoalRow, Lead, Voucher};
use crate::model::MemoryCharge;

/// The family is busy from this much reported CPU counter growth, millicores: the Spotlight
/// activity's own `CpuAtLeast` bar ([`crate::explain::ACTIVITIES`] reads it here).
pub const CPU_BAR_MC: i64 = 200;

/// The family is busy from this much read from storage, bytes per second: the Spotlight
/// activity's own `ReadAtLeast` bar.
pub const READ_BAR_BPS: u64 = 5_000_000;

/// `mdutil -s -a` runs at most this often, ms, and while the family is busy or crossed a bar
/// since the last run, this often.
pub const VOLUMES_EVERY_MS: u64 = 60_000;

/// While the family stays under its bars, `mdutil -s -a` runs this often, ms.
pub const VOLUMES_QUIET_MS: u64 = 300_000;

/// A volume reading older than this is not shown, ms.
pub const VOLUMES_STALE_MS: u64 = 600_000;

/// A split reading of the family is carried to later readings for at most this long, ms on the
/// host's monotonic clock.
pub const READING_CARRIED_MS: u64 = 60_000;

/// What the host runs `mdutil` with: every local volume's status, which needs no root.
pub const MDUTIL_ARGS: [&str; 2] = ["-s", "-a"];

/// The family's signing identifiers, matched exactly and only under Apple's voucher (the
/// Spotlight activity's members, [`crate::explain::ACTIVITIES`], read them here): read with
/// `codesign -dv` on macOS 27.2.1 (26B131). No prefix match, which would let in a look-alike
/// such as `SpotlightSettingsIntentsExtension`. There is no plain `spotlightknowledged`.
pub const MEMBERS: &[&str] = &[
    "com.apple.mds",
    "com.apple.mds_stores",
    "com.apple.mdworker",
    "com.apple.mdworker_shared",
    "com.apple.corespotlightd",
    "com.apple.managedcorespotlightd",
    "com.apple.spotlightknowledged.graph",
    "com.apple.spotlightknowledged.importer",
    "com.apple.spotlightknowledged.updater",
];

/// One volume's indexing state, as `mdutil -s` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Indexing {
    /// `Indexing enabled.`
    Enabled,
    /// `Indexing disabled.` or `Indexing and searching disabled.`
    Disabled,
    /// Any other status line, as printed (`Error: unknown indexing state.`), or `""` where the
    /// volume's block held none. Never read as disabled.
    Unknown(String),
}

impl Indexing {
    /// The word `sys` prints: `on`, `off` or `unknown`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Enabled => "on",
            Self::Disabled => "off",
            Self::Unknown(_) => "unknown",
        }
    }

    /// The word `sys --json` carries: `enabled`, `disabled` or `unknown`.
    #[must_use]
    pub fn json_word(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// One volume `mdutil` reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Its mount point, as printed (`/System/Volumes/Data`).
    pub path: String,
    /// Its indexing state.
    pub indexing: Indexing,
}

/// The family's state, the `await sys spotlight=` levels in rank order. [`state`] reads them in
/// this order of precedence: indexing, then off, then idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Under both bars, and some volume read is not disabled (or none was read).
    Idle,
    /// At or over either bar, whatever the volumes read: the family is working.
    Indexing,
    /// Under both bars, and every volume read reads indexing disabled.
    Off,
}

impl State {
    /// The levels, by rank: what `await sys spotlight=` takes.
    pub const WORDS: [&'static str; 3] = ["idle", "indexing", "off"];

    /// Its word.
    #[must_use]
    pub fn word(self) -> &'static str {
        Self::WORDS[self.rank()]
    }

    /// Its rank in [`Self::WORDS`].
    #[must_use]
    pub fn rank(self) -> usize {
        match self {
            Self::Idle => 0,
            Self::Indexing => 1,
            Self::Off => 2,
        }
    }
}

/// One family coalition's share of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    /// Its lead's signing identifier, one of [`MEMBERS`].
    pub signing_id: &'static str,
    /// Its lead's pid.
    pub lead_pid: u32,
    /// Its coalition's reported CPU counter growth over the window, millicores.
    pub cpu_mc: i64,
    /// What it read from storage, bytes per second.
    pub read_bps: u64,
    /// Its live members' footprint, bytes, where read.
    pub footprint: Option<u64>,
}

/// The family over one split window, with the volumes as last read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The split's wall clock, unix ms.
    pub at_unix_ms: i64,
    /// The window, ms.
    pub window_ms: u64,
    /// The family's reported CPU counter growth over the window, millicores: published
    /// counters, which neither floor nor bound the CPU it ran inside the window
    /// ([`crate::published_cpu`]).
    pub cpu_mc: i64,
    /// What it read from storage, bytes per second.
    pub read_bps: u64,
    /// Its footprint: the shares' footprints summed, with how many shares were unread.
    pub footprint: MemoryCharge,
    /// Each coalition's share, busiest first.
    pub shares: Vec<Share>,
    /// Each volume `mdutil -s -a` reported, where read and no older than
    /// [`VOLUMES_STALE_MS`].
    pub volumes: Option<Vec<Volume>>,
    /// How old [`Self::volumes`] is at the reading, ms.
    pub volumes_age_ms: Option<u64>,
    /// Its state ([`state`]).
    pub state: State,
}

/// The volumes `mdutil -s -a` (or `mdutil -s <volume>`) printed: each line that does not start
/// with whitespace and ends with `:` opens a volume's block, and each other line, trimmed, is
/// that volume's status. A block of no status line reads [`Indexing::Unknown`] `""`; one of
/// several that all read the same state reads that state, and one whose status lines differ
/// reads [`Indexing::Unknown`] holding its lines joined by `; `, never disabled.
///
/// # Errors
/// Nothing was printed, or a status line came before any volume.
pub fn volumes(text: &str) -> Result<Vec<Volume>, String> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let header = !line.starts_with(char::is_whitespace) && line.trim_end().ends_with(':');
        if header {
            let path = line.trim_end().trim_end_matches(':').to_owned();
            out.push((path, Vec::new()));
        } else {
            let Some((_, status)) = out.last_mut() else {
                return Err(format!(
                    "mdutil: a status before any volume: {}",
                    line.trim()
                ));
            };
            status.push(line.trim().to_owned());
        }
    }
    if out.is_empty() {
        return Err("mdutil printed no volume".into());
    }
    Ok(out
        .into_iter()
        .map(|(path, lines)| {
            let states: Vec<Indexing> = lines.iter().map(|l| status(l)).collect();
            let indexing = match states.as_slice() {
                [] => Indexing::Unknown(String::new()),
                [one] => one.clone(),
                many if many.iter().all(|s| *s == many[0]) => many[0].clone(),
                _ => Indexing::Unknown(lines.join("; ")),
            };
            Volume { path, indexing }
        })
        .collect())
}

fn status(line: &str) -> Indexing {
    match line {
        "Indexing enabled." => Indexing::Enabled,
        "Indexing disabled." | "Indexing and searching disabled." => Indexing::Disabled,
        other => Indexing::Unknown(other.to_owned()),
    }
}

/// Whether the family's `cpu_mc` and `read_bps` are at or over either bar.
#[must_use]
pub fn over_a_bar(cpu_mc: i64, read_bps: u64) -> bool {
    cpu_mc >= CPU_BAR_MC || read_bps >= READ_BAR_BPS
}

/// The family's state: [`State::Indexing`] at or over either bar, whatever the volumes read (a
/// busy family is working, and `corespotlightd` and `spotlightknowledged` index apart from the
/// volumes' indexing); else [`State::Off`] where volumes were read, there are some, and every
/// one reads disabled; else [`State::Idle`]. Volumes unread never make it off.
#[must_use]
pub fn state(cpu_mc: i64, read_bps: u64, volumes: Option<&[Volume]>) -> State {
    if over_a_bar(cpu_mc, read_bps) {
        State::Indexing
    } else if volumes
        .is_some_and(|v| !v.is_empty() && v.iter().all(|v| v.indexing == Indexing::Disabled))
    {
        State::Off
    } else {
        State::Idle
    }
}

/// The volumes as last read: what they were and when, ms on the host's monotonic clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumesRead {
    /// Each volume.
    pub volumes: Vec<Volume>,
    /// When `mdutil` ran, ms.
    pub at_ms: u64,
}

/// The family over a split's coalition `rows` (the window `window_ms` long ending at
/// `at_unix_ms`), with `volumes` as last read and `now_ms` the reading's monotonic clock. A
/// row counts where [`family_lead`] names it the family's.
#[must_use]
pub fn reading(
    rows: &[CoalRow],
    volumes: Option<&VolumesRead>,
    now_ms: u64,
    at_unix_ms: i64,
    window_ms: u64,
) -> Reading {
    reading_of(rows, MEMBERS, volumes, now_ms, at_unix_ms, window_ms)
}

/// The lead of `r` where the row is the family's whose signing ids are `members`: its
/// ownership is not mixed (a mixed coalition's growth has no single owner), and its lead is
/// Apple-signed under one of `members`, matched exactly.
fn family_lead<'r>(r: &'r CoalRow, members: &[&str]) -> Option<&'r Lead> {
    r.lead.as_ref().filter(|l| {
        !r.ownership_mixed
            && l.voucher == Voucher::Apple
            && l.signing_id
                .as_deref()
                .is_some_and(|id| members.contains(&id))
    })
}

/// [`reading`] of the family whose signing ids are `members`.
pub(crate) fn reading_of(
    rows: &[CoalRow],
    members: &[&'static str],
    volumes: Option<&VolumesRead>,
    now_ms: u64,
    at_unix_ms: i64,
    window_ms: u64,
) -> Reading {
    let mut shares: Vec<Share> = rows
        .iter()
        .filter_map(|r| {
            let lead = family_lead(r, members)?;
            let id = lead.signing_id.as_deref()?;
            let signing_id = members.iter().copied().find(|m| *m == id)?;
            Some(Share {
                signing_id,
                lead_pid: lead.pid,
                cpu_mc: r.cpu_mc,
                read_bps: r.read_bps,
                footprint: r.footprint,
            })
        })
        .collect();
    shares.sort_by(|a, b| b.cpu_mc.cmp(&a.cpu_mc).then(a.lead_pid.cmp(&b.lead_pid)));
    let cpu_mc = shares.iter().map(|s| s.cpu_mc).sum();
    let read_bps = shares
        .iter()
        .fold(0u64, |t, s| t.saturating_add(s.read_bps));
    let mut footprint = MemoryCharge::default();
    for s in &shares {
        footprint.add(s.footprint);
    }
    Reading {
        at_unix_ms,
        window_ms,
        cpu_mc,
        read_bps,
        footprint,
        shares,
        volumes: None,
        volumes_age_ms: None,
        state: State::Idle,
    }
    .with_volumes(volumes, now_ms)
}

/// The port `mdutil` is run through, with [`MDUTIL_ARGS`]: the host's live arm runs it under a
/// short deadline; a test's arm counts.
#[derive(Clone, Copy, Debug)]
pub struct MdutilProbe {
    /// What `mdutil -s -a` printed, from a run that exited 0; one line why not otherwise.
    pub status_all: fn() -> Result<String, String>,
}

impl MdutilProbe {
    /// Runs nothing: every volume stays unread.
    pub const INERT: Self = Self {
        status_all: || Err("inert".into()),
    };
}

/// When `mdutil -s -a` runs: at the first reading, then at most once every
/// [`VOLUMES_EVERY_MS`] while the family is busy or crossed a bar since the last run, and every
/// [`VOLUMES_QUIET_MS`] while it stays quiet. Never twice inside [`VOLUMES_EVERY_MS`].
#[derive(Clone, Debug)]
pub struct Cadence {
    /// The floor between runs, ms: [`VOLUMES_EVERY_MS`].
    every_ms: u64,
    /// The gap between runs while quiet, ms: [`VOLUMES_QUIET_MS`].
    quiet_ms: u64,
    last_run_ms: Option<u64>,
    crossed: bool,
    last: Option<VolumesRead>,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            every_ms: VOLUMES_EVERY_MS,
            quiet_ms: VOLUMES_QUIET_MS,
            last_run_ms: None,
            crossed: false,
            last: None,
        }
    }
}

impl Cadence {
    /// Whether a run is due at `now_ms`.
    #[must_use]
    pub fn due(&self, now_ms: u64) -> bool {
        let Some(last) = self.last_run_ms else {
            return true;
        };
        let since = now_ms.saturating_sub(last);
        since >= self.every_ms && (self.crossed || since >= self.quiet_ms)
    }

    /// Notes the family `busy` (at or over a bar) at `now_ms`, runs `probe` where a run is due,
    /// and returns the volumes as last read. A run that fails or prints what [`volumes`] cannot
    /// read leaves them unread until the next run.
    pub fn step(&mut self, now_ms: u64, busy: bool, probe: &MdutilProbe) -> Option<&VolumesRead> {
        self.crossed |= busy;
        if self.due(now_ms) {
            self.last_run_ms = Some(now_ms);
            self.crossed = false;
            self.last = (probe.status_all)()
                .and_then(|text| volumes(&text))
                .ok()
                .map(|volumes| VolumesRead {
                    volumes,
                    at_ms: now_ms,
                });
        }
        self.last.as_ref()
    }

    /// The volumes as last read.
    #[must_use]
    pub fn last(&self) -> Option<&VolumesRead> {
        self.last.as_ref()
    }
}

/// What the model keeps of the family between readings: the last split reading with the mono
/// ms it was made at, carried for [`READING_CARRIED_MS`], and the day's history.
#[derive(Clone, Debug, Default)]
pub struct Family {
    last: Option<(Reading, u64)>,
    history: crate::history::SpotlightHistory,
}

impl Family {
    /// The family at a reading of mono `now_ms`, with `volumes` as last read: from `fresh`'s
    /// rows where this reading made a split by coalition (which the history keeps), else the
    /// last split reading carried, its volumes refreshed; `None` before the first and once the
    /// last was made more than [`READING_CARRIED_MS`] before `now_ms` on the monotonic clock,
    /// so a wall clock set back never carries it longer.
    ///
    /// The history takes only split readings, which the host makes while a window in view
    /// shows the bars, or a `sys` query, a `sys why` or a spotlight wait asks for one: a span
    /// with none of those holds no bucket, which reads unread, never idle.
    pub fn absorb(
        &mut self,
        fresh: Option<&crate::explain::WhyFacts>,
        volumes: Option<&VolumesRead>,
        now_ms: u64,
    ) -> Option<Reading> {
        if let Some(f) = fresh {
            let r = reading(
                &f.split.rows,
                volumes,
                now_ms,
                f.at_unix_ms,
                f.window_ns / 1_000_000,
            );
            self.history.note(&r);
            self.last = Some((r, now_ms));
        } else {
            self.last = self
                .last
                .take()
                .filter(|(_, made_ms)| now_ms.saturating_sub(*made_ms) <= READING_CARRIED_MS)
                .map(|(r, made_ms)| (r.with_volumes(volumes, now_ms), made_ms));
        }
        self.last.as_ref().map(|(r, _)| r.clone())
    }

    /// Whether the last reading has the family at or over a bar.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.last
            .as_ref()
            .is_some_and(|(r, _)| over_a_bar(r.cpu_mc, r.read_bps))
    }

    /// The day's history.
    #[must_use]
    pub fn history(&self) -> &crate::history::SpotlightHistory {
        &self.history
    }

    /// Take the history the host kept in an earlier run.
    pub fn restore_history(&mut self, history: crate::history::SpotlightHistory) {
        self.history = history;
    }
}

impl Reading {
    /// The family's footprint, bytes, where every share's was read (`0` with no share); `None`
    /// where any was not, since the sum is then a lower bound.
    #[must_use]
    pub fn footprint_bytes(&self) -> Option<u64> {
        (self.footprint.unread == 0).then_some(self.footprint.bytes)
    }

    /// Whether no share's footprint was read, though there are shares.
    #[must_use]
    pub fn footprint_unread(&self) -> bool {
        self.footprint.members > 0 && self.footprint.unread == self.footprint.members
    }

    /// The same figures with `volumes` as read at mono `now_ms`, and the state they give.
    #[must_use]
    pub fn with_volumes(mut self, volumes: Option<&VolumesRead>, now_ms: u64) -> Self {
        let fresh = volumes.filter(|v| now_ms.saturating_sub(v.at_ms) <= VOLUMES_STALE_MS);
        self.volumes = fresh.map(|v| v.volumes.clone());
        self.volumes_age_ms = fresh.map(|v| now_ms.saturating_sub(v.at_ms));
        self.state = state(self.cpu_mc, self.read_bps, self.volumes.as_deref());
        self
    }
}

#[cfg(test)]
#[path = "spotlight_tests.rs"]
mod tests;
