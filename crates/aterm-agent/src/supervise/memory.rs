// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The memory ceiling: a supervised agent whose measured memory passes a
//! ceiling is warned about, and restarted only when the machine is in trouble
//! and this agent is what holds the memory.
//!
//! * Past the ceiling is a warning ([`judge`] → [`MemoryOver`]): one Messages
//!   row per episode ([`MemoryOver::warning`]), said again when what comes next
//!   changes ([`Verdict`]). The episode ends when the footprint falls an eighth
//!   under the ceiling ([`episode_over`]).
//! * A restart ([`MemoryOver::acts`]) only when the machine is in trouble
//!   ([`AgentMemory::strain`]: critical pressure, or warn while swapping at
//!   least [`THRASH_SWAP_KIB_S`]) and this agent dominates
//!   ([`AgentMemory::dominant`]). The loop then reads the measure as
//!   [`aterm_phase::WallKind::Memory`] at the next idle point, where the
//!   turn-end policy's `memory_restart` asks the host for a restart, or hands
//!   the person the resume line. One agent is restarted at a time, the largest.
//!
//! The ceiling ([`MemoryCeiling::ceiling_mib`]) is by default an eighth of
//! physical memory plus half of what is available now, clamped between the
//! smaller of [`AUTO_FLOOR_MIB`] and a quarter of the machine, and half of it.
//! A fixed number of GiB does not move; `off` disables warning and restart.

use std::fmt::Write as _;

/// The automatic ceiling's fixed part: one part in eight of the Mac.
pub const AUTO_BASE_SHARE: u64 = 8;

/// The automatic ceiling's moving part: one part in two of the memory
/// available now.
pub const AUTO_AVAILABLE_SHARE: u64 = 2;

/// The automatic ceiling's floor, MiB (4 GiB), where a quarter of the Mac is
/// more; a healthy Claude Code uses about 0.5 GiB.
pub const AUTO_FLOOR_MIB: u64 = 4 * 1024;

/// The automatic ceiling on a machine whose memory is unknown, MiB.
pub const UNKNOWN_MACHINE_MIB: u64 = 16 * 1024;

/// The swap rate, KiB/s (in plus out), at which a Mac whose kernel says warn
/// is thrashing ([`AgentMemory::strain`]): the strain engine's own warn enter
/// line, 4 MB/s. Machines frozen by memory swap tens of MB/s at warn.
pub use aterm_messages::strain::WARN_SWAP_ENTER_KIB_S as THRASH_SWAP_KIB_S;

/// The share of memory in use, percent, an agent must hold to be the dominant
/// consumer ([`AgentMemory::dominant`]): a fifth.
pub const DOMINANT_SHARE_PCT: u64 = 20;

/// How often a host reads an agent's memory at most: the loop asks at each
/// point and each step of its waits, and the host answers from its last
/// reading until this has passed.
pub const SAMPLE_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// `[harness] memory_ceiling`: how much memory a supervised agent may use
/// before it is warned about (and, when the Mac is in trouble and it holds
/// the most, restarted at its next idle point).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemoryCeiling {
    /// Moves with the machine ([`MemoryCeiling::ceiling_mib`]). The default.
    #[default]
    Auto,
    /// Never measured: no warning and no restart; only Claude Code's own
    /// banner is a wall. The key's limit.
    Off,
    /// This many GiB, whatever the machine.
    Gib(u32),
}

impl MemoryCeiling {
    /// Read `text`: `auto`; `off` (or `false`, or `0`); a whole number of GiB,
    /// bare or with a unit (`48`, `48GB`, `48 GiB`, `48g`). `None`: none of
    /// those.
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

    /// Whether `self` gives no more power than `before`: the same, off, or a
    /// HIGHER number of GiB (a higher ceiling warns, and restarts, less).
    /// `auto` against a number cannot be compared without the machine, so
    /// neither limits the other.
    #[must_use]
    pub fn no_more_than(self, before: Self) -> bool {
        match (self, before) {
            (a, b) if a == b => true,
            (Self::Off, _) => true,
            (Self::Gib(a), Self::Gib(b)) => a >= b,
            _ => false,
        }
    }

    /// The ceiling in MiB on a machine with `physical_mib` of memory and
    /// `available_mib` of it available now (`None`: unknown), or `None` when
    /// off. `auto`: an eighth of the memory plus half the available, never
    /// under the smaller of [`AUTO_FLOOR_MIB`] and a quarter of the memory,
    /// never over half of it; a quarter where the available is unknown;
    /// [`UNKNOWN_MACHINE_MIB`] where the memory is. A number of GiB is itself.
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

/// The kernel's memory pressure level (`kern.memorystatus_vm_pressure_level`
/// 1, 2, 4), as the host read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    /// Level 1.
    Normal,
    /// Level 2: the kernel is compressing and swapping to keep up.
    Warn,
    /// Level 4: the kernel is about to kill processes.
    Critical,
}

/// Why the machine is in trouble ([`AgentMemory::strain`]): what lets a
/// measure past the ceiling become a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strain {
    /// The kernel says its pressure is critical.
    Critical,
    /// The kernel says warn while the Mac swaps this many KiB/s (at least
    /// [`THRASH_SWAP_KIB_S`]): the thrash the recorded freezes were.
    Swapping {
        /// Swapped in plus out, KiB/s.
        swap_kib_s: u32,
    },
}

/// One reading of a supervised agent's memory and of the machine around it,
/// as its host measured it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentMemory {
    /// The physical footprint of the agent's process group, MiB
    /// (`aterm_sysprobe::GroupMemory`).
    pub footprint_mib: u64,
    /// The machine's physical memory, MiB; `None`: unknown.
    pub physical_mib: Option<u64>,
    /// The machine's memory available now — free, inactive and purgeable
    /// pages — MiB; `None`: unknown.
    pub available_mib: Option<u64>,
    /// The machine's memory in use (Activity Monitor's `Memory Used`), MiB;
    /// `None`: unknown.
    pub used_mib: Option<u64>,
    /// The kernel's memory pressure; `None`: unknown.
    pub pressure: Option<Pressure>,
    /// The machine's swap rate, KiB/s swapped in plus out
    /// (`aterm_sysprobe::SwapMeter`); `None`: not yet known.
    pub swap_kib_s: Option<u32>,
    /// The largest footprint among the other agents the same host
    /// supervises, measured lately, MiB; `None`: none measured.
    pub largest_other_mib: Option<u64>,
}

impl AgentMemory {
    /// Whether the machine is in trouble, and why: critical kernel pressure,
    /// or warn while swapping at least [`THRASH_SWAP_KIB_S`]. `None`: normal or
    /// unknown pressure, or warn with the swap under the line or unmeasured.
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

    /// Whether this agent is the dominant consumer: at least as large as every
    /// other agent its host supervises (measured lately), and at least
    /// [`DOMINANT_SHARE_PCT`] percent of memory in use (of the Mac's memory
    /// where use is unknown; never where both are).
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

/// An agent's memory past its ceiling ([`judge`]): a WARNING, and a restart
/// only where [`Self::acts`].
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

/// What a warning says comes next ([`MemoryOver::verdict`]). The warning is
/// said once an episode AND again whenever this changes within it — the Mac
/// falling into trouble, recovering, another agent growing past this one, a
/// host that restarts or no longer does, a restart the host refused (and one
/// made after it) — so the row on the window's
/// Messages (keyed per tab: the new one replaces the old) never says a
/// warning only while aterm restarts the agent, nor a restart that is no
/// longer coming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Acted on, and the host restarts it at its next idle point.
    Restart,
    /// Acted on, and no host restarts it (or the host refused the last
    /// restart asked for, and has made none since): the person's, with the
    /// resume line.
    Person,
    /// The Mac in trouble, another agent larger: a warning only.
    Behind,
    /// The Mac with memory to spare: a warning only.
    Spare,
}

/// Whether `m` is past the ceiling `cfg` sets: `Some` at or over it (a
/// warning, and a restart only where [`MemoryOver::acts`]), `None` under it
/// or with the ceiling off.
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

/// Whether a warning's EPISODE is over: no measure, the ceiling off, or the
/// footprint an eighth or more under the ceiling now. Between that line and
/// the ceiling the episode stands, so a ceiling that moves with the
/// machine's available memory does not warn again each time the two cross.
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

/// The swap rate as the strain row writes it, MB/s rounded.
fn mb_s(swap_kib_s: u32) -> u32 {
    swap_kib_s.saturating_add(512) / 1024
}

impl MemoryOver {
    /// Whether the measure is to be ACTED on — the agent restarted at its
    /// next idle point where its host can, the person handed its resume line
    /// where not: the machine is in trouble ([`AgentMemory::strain`]) and
    /// this agent is the dominant consumer. Otherwise it is a warning.
    #[must_use]
    pub fn acts(&self) -> bool {
        self.strain.is_some() && self.dominant
    }

    /// The measure, in the banner's shape (a fact, never a remedy): `Memory
    /// past aterm's ceiling (214.9 GB; ceiling 48.0 GB)`, with `, at
    /// critical memory pressure` or `, the Mac swapping 89 MB/s under memory
    /// pressure` where the machine is in trouble. No ` — `: the banner's
    /// head is read up to one ([`aterm_phase::memory_banner_head`]), and the
    /// memory wall carries this as its message.
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

    /// The machine's state, for a person: `the Mac has 60.2 GB available
    /// and is not short of memory`, or the trouble it is in.
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

    /// What the warning says comes next ([`Verdict`]), `restarts` being
    /// whether the host makes a restart: the part of a measure that, when it
    /// changes within an episode, has the warning said again — the numbers
    /// alone never do.
    #[must_use]
    pub fn verdict(&self, restarts: bool) -> Verdict {
        match (self.acts(), restarts) {
            (true, true) => Verdict::Restart,
            (true, false) => Verdict::Person,
            (false, _) if self.strain.is_some() => Verdict::Behind,
            (false, _) => Verdict::Spare,
        }
    }

    /// The WARNING said once an episode, and again whenever its
    /// [`Verdict`] changes within it (the window's Messages): `<agent>:
    /// <footprint and ceiling>; <the machine>; <what comes next>`. What
    /// comes next: aterm's restart at the next idle point where the measure
    /// [`Self::acts`] and `restarts` (the host makes one); the person's,
    /// with `resume` (the tab's own `claude --resume <id>`, or a Codex's
    /// `codex resume <thread>`), where it acts and the host makes none; else
    /// nothing — a warning — and the line that restarts it by hand. The line
    /// is the warning's last words, so a renderer that cuts keeps it whole
    /// or not at all ([`never_cut`]).
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

/// `render(text)` for a warning [`MemoryOver::warning`] made, `render` a
/// renderer that may cut its input to fit a cap or break it into lines (the
/// window's row: the tab's place ahead of it, 400 characters, lines at its
/// joints), with the resume line the warning ends with kept whole or not at
/// all — a rendering keeps it where it ends with it: the first of `text`,
/// `text` with each of the line's shorter forms
/// ([`crate::harness::resume::shorter`]: its permission mode alone, then the
/// conversation alone), and `text` naming no line, which still says to
/// restart it by hand. Text naming no line is rendered as it is. A cut id
/// names no conversation, or another's.
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
mod tests {
    use super::*;

    const GIB: u64 = 1024;

    /// The automatic ceiling moves with the machine: an eighth of the Mac plus
    /// half of what is available, between the smaller of 4 GiB and a quarter of
    /// the Mac, and half of the Mac; a quarter where available is unknown;
    /// 16 GiB on an unknown machine. Negative control: more available means a
    /// higher ceiling, and an owner's number does not move.
    #[test]
    fn the_automatic_ceiling_moves_with_the_available_memory() {
        let auto = |p: Option<u64>, a: Option<u64>| {
            MemoryCeiling::Auto.ceiling_mib(p.map(|g| g * GIB), a.map(|g| g * GIB))
        };
        // A 128 GB Mac.
        assert_eq!(auto(Some(128), Some(64)), Some(48 * GIB));
        assert_eq!(auto(Some(128), Some(10)), Some(21 * GIB));
        assert_eq!(auto(Some(128), Some(0)), Some(16 * GIB));
        assert_eq!(auto(Some(128), Some(128)), Some(64 * GIB), "capped at half");
        assert_eq!(
            auto(Some(128), Some(500)),
            Some(64 * GIB),
            "available past the Mac"
        );
        assert_eq!(auto(Some(128), None), Some(32 * GIB), "a quarter, unknown");
        // Small Macs: the floor.
        assert_eq!(auto(Some(16), Some(0)), Some(4 * GIB));
        assert_eq!(auto(Some(16), Some(8)), Some(6 * GIB));
        assert_eq!(
            auto(Some(8), Some(0)),
            Some(2 * GIB),
            "a quarter of a small Mac"
        );
        assert_eq!(auto(None, Some(10)), Some(16 * GIB));
        assert_eq!(auto(Some(0), None), Some(16 * GIB));
        // Negative controls.
        assert!(auto(Some(128), Some(40)) > auto(Some(128), Some(20)));
        let mine = MemoryCeiling::Gib(64);
        assert_eq!(mine.ceiling_mib(Some(128 * GIB), Some(0)), Some(64 * GIB));
        assert_eq!(
            mine.ceiling_mib(Some(128 * GIB), Some(100 * GIB)),
            Some(64 * GIB)
        );
        assert_eq!(
            MemoryCeiling::Off.ceiling_mib(Some(128 * GIB), Some(0)),
            None
        );
    }

    /// A reading of `gib_x10` tenths of a GiB on a `phys` GiB Mac with
    /// `avail` GiB available and `used` in use, at `pressure`, swapping
    /// `swap_mb_s` MB/s, alone under its host.
    fn m(
        gib_x10: u64,
        phys: u64,
        avail: u64,
        used: u64,
        pressure: Pressure,
        swap_mb_s: Option<u32>,
    ) -> AgentMemory {
        AgentMemory {
            footprint_mib: gib_x10 * GIB / 10,
            physical_mib: Some(phys * GIB),
            available_mib: Some(avail * GIB),
            used_mib: Some(used * GIB),
            pressure: Some(pressure),
            swap_kib_s: swap_mb_s.map(|s| s * 1024),
            largest_other_mib: None,
        }
    }

    /// The machine is in trouble at critical pressure whatever the swap, and at
    /// warn only while swapping at least [`THRASH_SWAP_KIB_S`] (e.g.
    /// `pressure warn · swap 89 MB/s`); not at warn with swap quiet, under the
    /// line or unmeasured, nor at normal pressure however fast it swaps, nor at
    /// an unknown level.
    #[test]
    fn the_machine_is_in_trouble_at_critical_or_at_warn_while_it_swaps() {
        let at = |p, swap| m(5, 128, 2, 120, p, swap).strain();
        assert_eq!(at(Pressure::Critical, None), Some(Strain::Critical));
        assert_eq!(at(Pressure::Critical, Some(0)), Some(Strain::Critical));
        for mb_s in [89, 58, 95, 4] {
            assert_eq!(
                at(Pressure::Warn, Some(mb_s)),
                Some(Strain::Swapping {
                    swap_kib_s: mb_s * 1024
                }),
                "{mb_s} MB/s"
            );
        }
        let under = AgentMemory {
            swap_kib_s: Some(THRASH_SWAP_KIB_S - 1),
            ..m(5, 128, 2, 120, Pressure::Warn, None)
        };
        assert_eq!(under.strain(), None, "under the line");
        assert_eq!(at(Pressure::Warn, Some(0)), None, "warn, quiet");
        assert_eq!(at(Pressure::Warn, None), None, "warn, not yet measured");
        assert_eq!(at(Pressure::Normal, Some(500)), None, "normal");
        let unknown = AgentMemory {
            pressure: None,
            ..m(5, 128, 2, 120, Pressure::Critical, Some(500))
        };
        assert_eq!(unknown.strain(), None, "an unknown level");
    }

    /// The dominant consumer: at least the largest other agent, and a fifth of
    /// memory in use. Negative controls: a larger agent beside it, a small
    /// share, and a machine whose use and size are both unknown.
    #[test]
    fn the_dominant_consumer_is_the_largest_and_a_fifth_of_the_memory_in_use() {
        let alone = m(280, 128, 2, 121, Pressure::Warn, Some(89));
        assert!(alone.dominant(), "28 of 121 GB: 23%");
        let tied = AgentMemory {
            largest_other_mib: Some(28 * GIB),
            ..alone
        };
        assert!(tied.dominant(), "as large as the largest other");
        // Negative controls.
        let beside = AgentMemory {
            largest_other_mib: Some(109 * GIB),
            ..alone
        };
        assert!(!beside.dominant(), "a larger agent beside it");
        assert!(
            !m(200, 128, 2, 121, Pressure::Warn, Some(89)).dominant(),
            "20 of 121: 16%"
        );
        let blind = AgentMemory {
            used_mib: None,
            physical_mib: None,
            ..alone
        };
        assert!(!blind.dominant(), "nothing to take a share of");
        let by_mac = AgentMemory {
            used_mib: None,
            ..alone
        };
        assert!(by_mac.dominant(), "28 of the 128 GB Mac: 21%");
    }

    /// Warn first: on a Mac with memory to spare every footprint past the
    /// ceiling is a warning, never acted on, a 215 GiB runaway included. Acted
    /// on only when the Mac is in trouble and the agent dominates (a 109 GB
    /// agent at warn swapping 89 MB/s with 121 GB in use; an agent at critical
    /// pressure). Negative controls: a 45.8 GB agent beside the 109 GB one is
    /// warned about, not acted on; a healthy agent is never over; `off` judges
    /// nothing; one MiB under the ceiling is under it.
    #[test]
    fn a_measure_past_the_ceiling_warns_and_acts_only_in_trouble_on_the_dominant_agent() {
        use Pressure::{Critical, Normal, Warn};
        let auto = MemoryCeiling::Auto;
        // Memory to spare: 60 GB available, normal pressure.
        let runaway = judge(auto, &m(2150, 128, 60, 68, Normal, Some(0))).expect("over");
        assert!(!runaway.acts(), "a warning only: {runaway:?}");
        assert!(runaway.dominant);
        assert_eq!(runaway.ceiling_mib, 46 * GIB);
        // Warn while quiet: the kernel keeps up; still a warning.
        let quiet = judge(auto, &m(2150, 128, 8, 120, Warn, Some(0))).expect("over");
        assert!(!quiet.acts(), "{quiet:?}");
        // The 09-28 freeze.
        let big = judge(
            auto,
            &AgentMemory {
                largest_other_mib: Some(458 * GIB / 10),
                ..m(1090, 128, 3, 121, Warn, Some(89))
            },
        )
        .expect("over");
        assert!(big.acts(), "{big:?}");
        let beside = judge(
            auto,
            &AgentMemory {
                largest_other_mib: Some(109 * GIB),
                ..m(458, 128, 3, 121, Warn, Some(89))
            },
        )
        .expect("over the ceiling too");
        assert!(!beside.acts(), "not the dominant one: {beside:?}");
        // The 09-24 process: 38.8 GiB on a 192 GB Mac at critical pressure.
        let crit = judge(auto, &m(388, 192, 4, 136, Critical, None)).expect("over");
        assert!(crit.acts(), "{crit:?}");
        // Negative controls.
        for phys in [8, 16, 32, 64, 128, 192] {
            assert!(
                judge(auto, &m(5, phys, 0, phys, Critical, None)).is_none(),
                "{phys} GB"
            );
        }
        assert!(judge(MemoryCeiling::Off, &m(2150, 128, 0, 128, Critical, None)).is_none());
        let at = AgentMemory {
            footprint_mib: 16 * GIB,
            ..m(0, 128, 0, 120, Critical, None)
        };
        assert!(judge(auto, &at).is_some(), "at the ceiling is over it");
        let under = AgentMemory {
            footprint_mib: 16 * GIB - 1,
            ..at
        };
        assert!(judge(auto, &under).is_none());
    }

    /// The episode ends an eighth under the ceiling, so a moving ceiling does
    /// not warn again each time it crosses the footprint. Negative controls:
    /// just under the ceiling the episode stands; no measure and `off` end it.
    #[test]
    fn a_warning_episode_ends_an_eighth_under_the_ceiling() {
        let auto = MemoryCeiling::Auto;
        // 128 GB Mac, nothing available: a 16 GiB ceiling, 14 GiB line.
        let at = |mib: u64| AgentMemory {
            footprint_mib: mib,
            ..m(0, 128, 0, 120, Pressure::Normal, None)
        };
        assert!(!episode_over(auto, Some(&at(16 * GIB))));
        assert!(
            !episode_over(auto, Some(&at(15 * GIB))),
            "under, within the band"
        );
        assert!(!episode_over(auto, Some(&at(14 * GIB))));
        assert!(episode_over(auto, Some(&at(14 * GIB - 1))));
        assert!(episode_over(auto, None));
        assert!(episode_over(MemoryCeiling::Off, Some(&at(200 * GIB))));
    }

    /// The verdict is what has the warning said again: it moves with the Mac's
    /// trouble, this agent's rank and a restarting host, not with the numbers,
    /// so a footprint, ceiling or swap rate moving within an episode says
    /// nothing new.
    #[test]
    fn the_verdict_moves_with_trouble_rank_and_restart_not_with_the_numbers() {
        let spare = MemoryOver {
            footprint_mib: 220_279,
            ceiling_mib: 48 * GIB,
            available_mib: Some(60 * GIB),
            strain: None,
            dominant: true,
        };
        let trouble = MemoryOver {
            strain: Some(Strain::Swapping {
                swap_kib_s: 89 * 1024,
            }),
            ..spare
        };
        assert_eq!(spare.verdict(true), Verdict::Spare);
        assert_eq!(spare.verdict(false), Verdict::Spare);
        assert_eq!(trouble.verdict(true), Verdict::Restart);
        assert_eq!(trouble.verdict(false), Verdict::Person);
        let behind = MemoryOver {
            dominant: false,
            ..trouble
        };
        assert_eq!(behind.verdict(true), Verdict::Behind);
        // Negative control: other numbers, the same verdicts.
        let moved = MemoryOver {
            footprint_mib: 300_000,
            ceiling_mib: 17 * GIB,
            available_mib: Some(2 * GIB),
            strain: Some(Strain::Critical),
            dominant: true,
        };
        assert_eq!(moved.verdict(true), trouble.verdict(true));
        assert_eq!(
            MemoryOver {
                available_mib: Some(10 * GIB),
                ..spare
            }
            .verdict(true),
            spare.verdict(true)
        );
    }

    /// The fact is in the banner's shape, with no ` — ` for the banner's head
    /// to cut at; the warning names the footprint, the machine and the
    /// resume line; what comes next follows the verdict.
    #[test]
    fn the_fact_and_the_warning_name_the_measure_the_machine_and_the_remedy() {
        let over = MemoryOver {
            footprint_mib: 220_279,
            ceiling_mib: 48 * GIB,
            available_mib: Some(60 * GIB),
            strain: None,
            dominant: true,
        };
        assert_eq!(
            over.fact(),
            "Memory past aterm's ceiling (215.1 GB; ceiling 48.0 GB)"
        );
        let resume = "claude --resume 0badf00d";
        assert_eq!(
            over.warning("Claude Code", true, Some(resume)),
            "Claude Code uses 215.1 GB, past aterm's memory ceiling of 48.0 GB; the Mac has \
             60.0 GB available and is not short of memory; a warning only: nothing is \
             restarted; to restart it yourself: claude --resume 0badf00d"
        );
        let swapping = MemoryOver {
            strain: Some(Strain::Swapping { swap_kib_s: 60_000 }),
            ..over
        };
        assert!(
            swapping
                .fact()
                .ends_with(", the Mac swapping 59 MB/s under memory pressure)"),
            "{}",
            swapping.fact()
        );
        assert!(
            swapping
                .warning("Claude Code", true, Some(resume))
                .ends_with("aterm restarts it on its conversation at its next idle point"),
        );
        assert!(
            swapping
                .warning("Claude Code", false, Some(resume))
                .ends_with(
                    "it holds the most memory; to restart it yourself: claude --resume 0badf00d"
                ),
        );
        let second = MemoryOver {
            dominant: false,
            ..swapping
        };
        assert!(
            second
                .warning("Codex", true, None)
                .contains("a warning only: another agent holds more, so it keeps running"),
        );
        let critical = MemoryOver {
            strain: Some(Strain::Critical),
            ..over
        };
        assert!(critical.fact().ends_with(", at critical memory pressure)"));
        for f in [over, swapping, critical] {
            assert_eq!(aterm_phase::memory_banner_head(&f.fact()), f.fact());
        }
        assert_eq!(gb(0), "0.0 GB");
        assert_eq!(gb(1536), "1.5 GB");
    }

    /// THE WARNING'S RESUME LINE IS KEPT WHOLE OR NOT AT ALL ([`never_cut`]):
    /// under a renderer's cap, a Codex's line steps down to its sandbox and
    /// approval flags, then to its thread alone, and past that the warning
    /// names no line and says to restart it by hand. A warning that names
    /// none is rendered as it is. NEGATIVE CONTROL: the cap alone cuts the
    /// thread.
    #[test]
    fn the_warnings_resume_line_is_kept_whole_or_not_at_all() {
        let over = MemoryOver {
            footprint_mib: 51_302,
            ceiling_mib: 24 * GIB,
            available_mib: Some(12_595),
            strain: None,
            dominant: true,
        };
        let thread = "019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b";
        let argv: Vec<String> = [
            "codex",
            "-m",
            "gpt-5-codex",
            "-c",
            "mcp_servers.github.command=\"/opt/homebrew/bin/github-mcp-server\"",
            "-s",
            "workspace-write",
            "-a",
            "on-request",
        ]
        .map(String::from)
        .to_vec();
        let line = crate::harness::resume::codex_command(&argv, thread, None).unwrap();
        let text = over.warning("Codex", false, Some(&line));
        let cap = |n: usize| {
            move |t: &str| -> String {
                let mut out: String = t.chars().take(n).collect();
                if t.chars().count() > n {
                    out.push('…');
                }
                out
            }
        };
        let len = |t: &str| t.chars().count();
        let head = text.strip_suffix(line.as_str()).unwrap();
        let mode = format!("{head}codex resume -s workspace-write -a on-request {thread}");
        let bare = format!("{head}codex resume {thread}");
        assert_eq!(never_cut(&text, cap(len(&text))), text, "whole");
        assert_eq!(never_cut(&text, cap(len(&text) - 1)), mode);
        assert_eq!(never_cut(&text, cap(len(&mode) - 1)), bare);
        let by_hand = never_cut(&text, cap(len(&bare) - 1));
        assert!(
            by_hand.ends_with(
                "nothing is restarted; restart it yourself if it should give the memory back"
            ),
            "{by_hand}"
        );
        let restart = MemoryOver {
            strain: Some(Strain::Critical),
            ..over
        }
        .warning("Codex", true, Some(&line));
        assert_eq!(never_cut(&restart, cap(400)), restart, "no line named");
        // NEGATIVE CONTROL.
        assert!(!cap(len(&text) - 1)(&text).contains(thread));
    }

    /// The key's text round-trips, takes a unit, and reads zero as off.
    /// Negative control: a fraction, a negative and a word are no value.
    #[test]
    fn the_key_reads_auto_off_and_gibibytes() {
        for (text, want) in [
            ("auto", MemoryCeiling::Auto),
            (" AUTO ", MemoryCeiling::Auto),
            ("off", MemoryCeiling::Off),
            ("false", MemoryCeiling::Off),
            ("0", MemoryCeiling::Off),
            ("0GB", MemoryCeiling::Off),
            ("48", MemoryCeiling::Gib(48)),
            ("48GB", MemoryCeiling::Gib(48)),
            ("48 GiB", MemoryCeiling::Gib(48)),
            ("48g", MemoryCeiling::Gib(48)),
        ] {
            assert_eq!(MemoryCeiling::parse(text), Some(want), "{text:?}");
            assert_eq!(MemoryCeiling::parse(&want.text()), Some(want));
        }
        for bad in ["1.5", "-4", "lots", "", "GB"] {
            assert_eq!(MemoryCeiling::parse(bad), None, "{bad:?}");
        }
    }

    /// What limits: off below everything, a higher number below a lower,
    /// the same as itself. `auto` and a number are not comparable.
    #[test]
    fn a_higher_ceiling_or_off_is_the_limit() {
        use MemoryCeiling::{Auto, Gib, Off};
        assert!(Off.no_more_than(Auto) && Off.no_more_than(Gib(8)));
        assert!(Gib(64).no_more_than(Gib(32)) && Gib(32).no_more_than(Gib(32)));
        assert!(!Gib(16).no_more_than(Gib(32)));
        assert!(!Auto.no_more_than(Off) && !Gib(8).no_more_than(Off));
        assert!(!Auto.no_more_than(Gib(8)) && !Gib(8).no_more_than(Auto));
    }
}
