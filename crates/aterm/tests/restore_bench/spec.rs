// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The restore measurement's grammar, std-only: the words of one run in `RESTORE_BENCH`, which
//! `verify perf restore-e2e` mounts to refuse what the bench would before it builds anything.
//!
//! `out=<dir>` (required) · `sessions=1,8,32` (rising, each at most [`SESSIONS_MAX`]) ·
//! `rounds=3` (crashes and recoveries per session count, at most [`ROUNDS_MAX`]) · `lines=5000`
//! (the history each session prints, at most [`LINES_MAX`]) · `arm=clean|copy|lookalike` ·
//! `lookalike_kib=6912` (the look-alike arm's plant, at most [`LOOKALIKE_KIB_MAX`]; only with
//! `arm=lookalike`).

use std::path::PathBuf;

/// The environment variable a run's words arrive in.
pub const ENV: &str = "RESTORE_BENCH";
/// The most sessions one crash takes down.
pub const SESSIONS_MAX: u32 = 64;
/// The most crashes per session count.
pub const ROUNDS_MAX: u32 = 20;
/// The most history lines one session prints.
pub const LINES_MAX: u32 = 100_000;
/// The largest plant, in KiB: a v1 ring holds about 6.9 MiB, and the plant stops at its lap.
pub const LOOKALIKE_KIB_MAX: u32 = 8192;
/// The default plant: the full v1 ring past its chunks, capped at the lap (`resync_walk`'s
/// largest case).
pub const LOOKALIKE_KIB_DEFAULT: u32 = 6912;

/// The recovering window's seam the copy and look-alike arms set
/// (`crash_restore::LOOKALIKE_SEAM`). Every round checks the seam took in each offered region
/// (`run::arm_took`, from the window's own `metrics`), so a name drifted from the window's
/// fails the run rather than measuring the clean arm under another arm's name.
pub const LOOKALIKE_SEAM: &str = "ATERM_DEBUG_CRASH_RESTORE_LOOKALIKE_KIB";

/// What the recovering window restores each offered region from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Arm {
    /// The keeper's region itself: what ships.
    #[default]
    Clean,
    /// A copy of the region in the window's heap, its making off the window's clocks: against
    /// the clean arm, what restoring from a heap copy rather than the mapped region changes.
    Copy,
    /// That copy with the look-alike tail planted (the newest history chunk damaged, frame
    /// look-alikes past the ring's commit), the plant off the window's clocks too: the damage
    /// the ring walk's resync bound was built for, the costliest tail in an optimized build (a
    /// random tail costs a dev build's walk more, `benches/resync_walk.rs`).
    Lookalike,
}

impl Arm {
    /// Every arm, in the order `--arm` lists them.
    pub const ALL: [Arm; 3] = [Arm::Clean, Arm::Copy, Arm::Lookalike];

    /// The arm's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Arm::Clean => "clean",
            Arm::Copy => "copy",
            Arm::Lookalike => "lookalike",
        }
    }

    /// The arm `raw` names.
    ///
    /// # Errors
    /// Anything but an arm's [`word`](Arm::word).
    pub fn parse(raw: &str) -> Result<Arm, String> {
        Arm::ALL
            .into_iter()
            .find(|a| a.word() == raw)
            .ok_or_else(|| format!("`arm` is clean, copy or lookalike, not `{raw}`"))
    }

    /// The value the recovering window's [`LOOKALIKE_SEAM`] takes, or `None` to leave it unset.
    #[must_use]
    pub fn seam(self, kib: u32) -> Option<String> {
        match self {
            Arm::Clean => None,
            Arm::Copy => Some("0".to_string()),
            Arm::Lookalike => Some(kib.to_string()),
        }
    }
}

/// The experimental operations the recovering process actually completed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Copies selected after the complete requested experiment succeeded.
    pub copies: u64,
    /// Those copies whose requested look-alike damage was applied.
    pub plants: u64,
    /// Requests that fell back to the untouched original region.
    pub refused: u64,
}

impl Applied {
    /// Every session received the requested arm, with no refused or unrelated experiment.
    #[must_use]
    pub fn matches(self, arm: Arm, sessions: u64) -> bool {
        if sessions == 0 || self.refused != 0 {
            return false;
        }
        let (copies, plants) = match arm {
            Arm::Clean => (0, 0),
            Arm::Copy => (sessions, 0),
            Arm::Lookalike => (sessions, sessions),
        };
        self.copies == copies && self.plants == plants
    }
}

/// One run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub out: PathBuf,
    pub sessions: Vec<u32>,
    pub rounds: u32,
    pub lines: u32,
    pub arm: Arm,
    pub lookalike_kib: u32,
}

impl Spec {
    /// The run's words as the report spells them (every knob, defaults included; `out=` left
    /// out, a scratch path).
    #[must_use]
    pub fn words(&self) -> String {
        let sessions: Vec<String> = self.sessions.iter().map(u32::to_string).collect();
        let mut words = format!(
            "sessions={} rounds={} lines={} arm={}",
            sessions.join(","),
            self.rounds,
            self.lines,
            self.arm.word()
        );
        if self.arm == Arm::Lookalike {
            words.push_str(&format!(" lookalike_kib={}", self.lookalike_kib));
        }
        words
    }
}

fn bounded(key: &str, raw: &str, max: u32) -> Result<u32, String> {
    match raw.parse::<u32>() {
        Ok(v) if (1..=max).contains(&v) => Ok(v),
        _ => Err(format!("`{key}` is 1 to {max}, not `{raw}`")),
    }
}

/// Parse a run's words.
///
/// # Errors
/// An unknown or repeated word, a value out of its bounds, a session list that does not rise,
/// `lookalike_kib=` beside another arm, or no `out=`.
pub fn parse(words: &str) -> Result<Spec, String> {
    let mut out = None;
    let mut sessions = None;
    let mut rounds = None;
    let mut lines = None;
    let mut arm = None;
    let mut kib = None;
    for word in words.split_whitespace() {
        let (key, value) = word
            .split_once('=')
            .ok_or_else(|| format!("`{word}` is not key=value"))?;
        let fresh = match key {
            "out" => out.replace(PathBuf::from(value)).is_none(),
            "sessions" => {
                let list = value
                    .split(',')
                    .map(|n| bounded(key, n, SESSIONS_MAX))
                    .collect::<Result<Vec<u32>, String>>()?;
                if list.windows(2).any(|w| w[0] >= w[1]) {
                    return Err(format!("`sessions` rises, and `{value}` does not"));
                }
                sessions.replace(list).is_none()
            }
            "rounds" => rounds.replace(bounded(key, value, ROUNDS_MAX)?).is_none(),
            "lines" => lines.replace(bounded(key, value, LINES_MAX)?).is_none(),
            "arm" => arm.replace(Arm::parse(value)?).is_none(),
            "lookalike_kib" => kib
                .replace(bounded(key, value, LOOKALIKE_KIB_MAX)?)
                .is_none(),
            _ => return Err(format!("unknown word `{key}`")),
        };
        if !fresh {
            return Err(format!("`{key}` given twice"));
        }
    }
    let arm = arm.unwrap_or_default();
    if kib.is_some() && arm != Arm::Lookalike {
        return Err("`lookalike_kib` is the lookalike arm's".to_string());
    }
    Ok(Spec {
        out: out.ok_or("`out=` names the report's folder")?,
        sessions: sessions.unwrap_or_else(|| vec![1, 8, 32]),
        rounds: rounds.unwrap_or(3),
        lines: lines.unwrap_or(5_000),
        arm,
        lookalike_kib: kib.unwrap_or(LOOKALIKE_KIB_DEFAULT),
    })
}

/// The run [`ENV`] names.
///
/// # Errors
/// [`ENV`] unset, or words [`parse`] refuses.
pub fn from_env() -> Result<Spec, String> {
    let words = std::env::var(ENV).map_err(|_| {
        format!("{ENV} is unset: `targo --unverified verify perf restore-e2e` runs this bench")
    })?;
    parse(&words)
}
