// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The memory ceiling.

use std::fmt::Write as _;

/// The automatic ceiling's fixed part: one part in eight of the Mac.
pub const AUTO_BASE_SHARE: u64 = 8;

/// The automatic ceiling's moving part: one part in two of the memory available now.
pub const AUTO_AVAILABLE_SHARE: u64 = 2;

/// The automatic ceiling's floor, MiB (4 GiB), where a quarter of the Mac is more.
pub const AUTO_FLOOR_MIB: u64 = 4 * 1024;

/// The automatic ceiling on a machine whose memory is unknown, MiB.
pub const UNKNOWN_MACHINE_MIB: u64 = 16 * 1024;

/// The swap rate, KiB/s, at which a Mac whose kernel says warn is thrashing: 4 MB/s.
pub const THRASH_SWAP_KIB_S: u32 = 4096;

/// The share of memory in use, percent, an agent must hold to be the dominant consumer: a fifth.
pub const DOMINANT_SHARE_PCT: u64 = 20;

/// How often a host reads an agent's memory at most.
pub const SAMPLE_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// `[harness] memory_ceiling`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemoryCeiling {
    /// Moves with the machine ([`MemoryCeiling::ceiling_mib`]).
    #[default]
    Auto,
    /// Never measured: no warning and no restart; only Claude Code's own banner is a wall.
    Off,
    /// This many GiB, whatever the machine.
    Gib(u32),
}

impl MemoryCeiling {
    /// Read `text`: `auto`; `off`; a whole number of GiB, bare or with a unit.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim().to_ascii_lowercase();
        match t.as_str() {
            "auto" => return Some(Self::Auto),
            "off" | "false" | "0" => return Some(Self::Off),
            _ => {}
        }
        let digits = ["gib", "gb", "g"]
            .iter()
            .find_map(|unit| t.strip_suffix(unit))
            .unwrap_or(&t)
            .trim();
        match digits.parse::<u32>() {
            Ok(0) => Some(Self::Off),
            Ok(n) => Some(Self::Gib(n)),
            Err(_) => None,
        }
    }

    /// The text [`Self::parse`] takes back: `auto`, `off` or the GiB.
    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Auto => "auto".to_string(),
            Self::Off => "off".to_string(),
            Self::Gib(n) => n.to_string(),
        }
    }

    /// Whether `self` gives no more power than `before`: the same, off, or a higher number of GiB.
    #[must_use]
    pub fn no_more_than(self, before: Self) -> bool {
        match (self, before) {
            (a, b) if a == b => true,
            (Self::Off, _) => true,
            (Self::Gib(a), Self::Gib(b)) => a >= b,
            _ => false,
        }
    }

    /// The ceiling in MiB on a machine with `physical_mib` of memory and `available_mib` of it,
    /// the memory `sys mem` calls available ([`AgentMemory::available_mib`]). That figure counts an idle
    /// anonymous page only by what compressing it frees; until 2026-11-06 the ceiling read free
    /// plus every inactive page plus purgeable, which counts anonymous pages the kernel gives back
    /// only by compressing or swapping them, so `Auto` is lower now by half the difference. The
    /// shares were left as they were: on a 24 GiB Mac at one look the old figure read 10.0 GiB and
    /// a ceiling of 8.0 GiB, the honest one 6.9 GiB and 6.4 GiB, and a ceiling that moves with
    /// what the machine can hand out without paging is the one the warning means.
    #[must_use]
    pub fn ceiling_mib(self, physical_mib: Option<u64>, available_mib: Option<u64>) -> Option<u64> {
        match self {
            Self::Off => None,
            Self::Gib(n) => Some(u64::from(n) * 1024),
            Self::Auto => {
                let Some(p) = physical_mib.filter(|p| *p > 0) else {
                    return Some(UNKNOWN_MACHINE_MIB);
                };
                let floor = AUTO_FLOOR_MIB.min(p / 4);
                let cap = p / 2;
                let moving = match available_mib {
                    Some(a) => p / AUTO_BASE_SHARE + a.min(p) / AUTO_AVAILABLE_SHARE,
                    None => p / 4,
                };
                Some(moving.clamp(floor, cap.max(floor)))
            }
        }
    }
}

/// The kernel's memory pressure level, as the host read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    /// Level 1.
    Normal,
    /// Level 2: the kernel is compressing and swapping to keep up.
    Warn,
    /// Level 4: the kernel is about to kill processes.
    Critical,
}

/// Why the machine is in trouble: what lets a measure past the ceiling become a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strain {
    /// The kernel says its pressure is critical.
    Critical,
    /// The kernel says warn while the Mac swaps this many KiB/s (at least [`THRASH_SWAP_KIB_S`]).
    Swapping {
        /// Swapped in plus out, KiB/s.
        swap_kib_s: u32,
    },
}

/// One reading of a supervised agent's memory and of the machine around it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentMemory {
    /// The physical footprint of the agent's process group, MiB (`aterm_sysprobe::GroupMemory`).
    pub footprint_mib: u64,
    /// The machine's physical memory, MiB; `None`: unknown.
    pub physical_mib: Option<u64>,
    /// The machine's memory available now, what the kernel can hand out without paging
    /// (`aterm_sysprobe::MemoryDetail::available`, the figure `sys mem` prints), MiB; `None`:
    /// unknown.
    pub available_mib: Option<u64>,
    /// The machine's memory in use (`aterm_sysprobe::MemoryDetail::used`, Activity Monitor's
    /// `Memory Used`), MiB; `None`: unknown.
    pub used_mib: Option<u64>,
    /// The kernel's memory pressure; `None`: unknown.
    pub pressure: Option<Pressure>,
    /// The machine's swap rate, KiB/s swapped in plus out; `None`: not yet known.
    pub swap_kib_s: Option<u32>,
    /// The largest footprint among the other agents the same host supervises, measured lately, MiB.
    pub largest_other_mib: Option<u64>,
}

impl AgentMemory {
    /// Whether the machine is in trouble, and why.
    #[must_use]
    pub fn strain(&self) -> Option<Strain> {
        match self.pressure? {
            Pressure::Critical => Some(Strain::Critical),
            Pressure::Warn => self
                .swap_kib_s
                .filter(|s| *s >= THRASH_SWAP_KIB_S)
                .map(|swap_kib_s| Strain::Swapping { swap_kib_s }),
            Pressure::Normal => None,
        }
    }

    /// Whether this agent is the dominant consumer.
    #[must_use]
    pub fn dominant(&self) -> bool {
        let largest = self
            .largest_other_mib
            .is_none_or(|other| self.footprint_mib >= other);
        let share = self
            .used_mib
            .or(self.physical_mib)
            .filter(|d| *d > 0)
            .is_some_and(|d| self.footprint_mib.saturating_mul(100) >= d * DOMINANT_SHARE_PCT);
        largest && share
    }
}

/// An agent's memory past its ceiling: a warning, and a restart only where [`Self::acts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryOver {
    /// What was measured, MiB.
    pub footprint_mib: u64,
    /// The ceiling it reached, MiB.
    pub ceiling_mib: u64,
    /// The machine's memory available at the reading, MiB; `None`: unknown.
    pub available_mib: Option<u64>,
    /// Why the machine is in trouble; `None`: it is not.
    pub strain: Option<Strain>,
    /// This agent is the dominant consumer ([`AgentMemory::dominant`]).
    pub dominant: bool,
}

/// What a warning says comes next ([`MemoryOver::verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Acted on, and the host restarts it at its next idle point.
    Restart,
    /// Acted on, and no host restarts it: the person's, with the resume line.
    Person,
    /// The Mac in trouble, another agent larger: a warning only.
    Behind,
    /// The Mac with memory to spare: a warning only.
    Spare,
}

/// Whether `m` is past the ceiling `cfg` sets.
#[must_use]
pub fn judge(cfg: MemoryCeiling, m: &AgentMemory) -> Option<MemoryOver> {
    let ceiling_mib = cfg.ceiling_mib(m.physical_mib, m.available_mib)?;
    (m.footprint_mib >= ceiling_mib).then(|| MemoryOver {
        footprint_mib: m.footprint_mib,
        ceiling_mib,
        available_mib: m.available_mib,
        strain: m.strain(),
        dominant: m.dominant(),
    })
}

/// Whether a warning's episode is over.
#[must_use]
pub fn episode_over(cfg: MemoryCeiling, m: Option<&AgentMemory>) -> bool {
    let Some(m) = m else {
        return true;
    };
    let Some(ceiling) = cfg.ceiling_mib(m.physical_mib, m.available_mib) else {
        return true;
    };
    m.footprint_mib < ceiling - ceiling / 8
}

/// `mib` as the banner writes a size: GiB with one decimal, `GB`.
#[must_use]
pub fn gb(mib: u64) -> String {
    let tenths = mib.saturating_mul(10) / 1024;
    format!("{}.{} GB", tenths / 10, tenths % 10)
}

/// The swap rate as the banner writes it, MB/s rounded.
const fn mb_s(swap_kib_s: u32) -> u32 {
    swap_kib_s.saturating_add(512) / 1024
}

impl MemoryOver {
    /// Whether the measure is acted on (a restart at the next idle point, or the person handed
    /// the resume line): the machine is in trouble and this agent is the dominant consumer.
    #[must_use]
    pub const fn acts(&self) -> bool {
        self.strain.is_some() && self.dominant
    }

    /// The measure, in the banner's shape (a fact, never a remedy).
    #[must_use]
    pub fn fact(&self) -> String {
        let mut s = format!(
            "Memory past aterm's ceiling ({}; ceiling {}",
            gb(self.footprint_mib),
            gb(self.ceiling_mib)
        );
        match self.strain {
            Some(Strain::Critical) => {
                let _ = write!(s, ", at critical memory pressure");
            }
            Some(Strain::Swapping { swap_kib_s }) => {
                let _ = write!(
                    s,
                    ", the Mac swapping {} MB/s under memory pressure",
                    mb_s(swap_kib_s)
                );
            }
            None => {}
        }
        s.push(')');
        s
    }

    /// The machine's state, for a person.
    #[must_use]
    pub fn machine(&self) -> String {
        match self.strain {
            Some(Strain::Critical) => "the Mac's memory pressure is critical".to_string(),
            Some(Strain::Swapping { swap_kib_s }) => format!(
                "the Mac swaps {} MB/s under memory pressure",
                mb_s(swap_kib_s)
            ),
            None => match self.available_mib {
                Some(a) => format!("the Mac has {} available and is not short of memory", gb(a)),
                None => "the Mac is not short of memory".to_string(),
            },
        }
    }

    /// What the warning says comes next, `restarts` being whether the host makes a restart.
    #[must_use]
    pub const fn verdict(&self, restarts: bool) -> Verdict {
        match (self.acts(), restarts) {
            (true, true) => Verdict::Restart,
            (true, false) => Verdict::Person,
            (false, _) if self.strain.is_some() => Verdict::Behind,
            (false, _) => Verdict::Spare,
        }
    }

    /// The warning for the window's Messages.
    #[must_use]
    pub fn warning(&self, agent: &str, restarts: bool, resume: Option<&str>) -> String {
        let (footprint, ceiling) = (gb(self.footprint_mib), gb(self.ceiling_mib));
        let machine = self.machine();
        let yourself = match resume {
            Some(line) => format!("{RESTART_IT_YOURSELF}{line}"),
            None => RESTART_IT_BY_HAND.to_string(),
        };
        let then = match self.verdict(restarts) {
            Verdict::Restart => {
                "it holds the most memory, so aterm restarts it on its conversation at its next \
                 idle point"
                    .to_string()
            }
            Verdict::Person => format!("it holds the most memory; {yourself}"),
            Verdict::Behind => {
                format!("a warning only: another agent holds more, so it keeps running; {yourself}")
            }
            Verdict::Spare => format!("a warning only: nothing is restarted; {yourself}"),
        };
        format!(
            "{agent} uses {footprint}, past aterm's memory ceiling of {ceiling}; {machine}; {then}"
        )
    }
}

/// How the warning hands the person the tab's resume line, its last words
/// ([`MemoryOver::warning`]); [`never_cut`] finds the line by it.
const RESTART_IT_YOURSELF: &str = "to restart it yourself: ";

/// What the warning says in its place where it can name no line.
const RESTART_IT_BY_HAND: &str = "restart it yourself if it should give the memory back";

/// `render(text)` for a warning [`MemoryOver::warning`] made.
#[must_use]
pub fn never_cut(text: &str, render: impl Fn(&str) -> String) -> String {
    let Some(at) = text.find(RESTART_IT_YOURSELF) else {
        return render(text);
    };
    let (head, line) = (&text[..at], &text[at + RESTART_IT_YOURSELF.len()..]);
    std::iter::once(line.to_string())
        .chain(crate::harness::resume::shorter(line))
        .find_map(|step| {
            let out = render(&format!("{head}{RESTART_IT_YOURSELF}{step}"));
            out.ends_with(&step).then_some(out)
        })
        .unwrap_or_else(|| render(&format!("{head}{RESTART_IT_BY_HAND}")))
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
