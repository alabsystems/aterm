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

    /// The ceiling in MiB on a machine with `physical_mib` of memory and `available_mib` of it.
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
    /// The machine's memory available now — free, inactive and purgeable pages — MiB; `None`.
    pub available_mib: Option<u64>,
    /// The machine's memory in use (Activity Monitor's `Memory Used`), MiB; `None`: unknown.
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
fn mb_s(swap_kib_s: u32) -> u32 {
    swap_kib_s.saturating_add(512) / 1024
}

impl MemoryOver {
    /// Whether the measure is acted on (a restart at the next idle point.
    #[must_use]
    pub fn acts(&self) -> bool {
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
    pub fn verdict(&self, restarts: bool) -> Verdict {
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
mod tests {
    use super::*;

    const GIB: u64 = 1024;

    /// The automatic ceiling follows [`MemoryCeiling::ceiling_mib`].
    #[test]
    fn the_automatic_ceiling_moves_with_the_available_memory() {
        let auto = |p: Option<u64>, a: Option<u64>| {
            MemoryCeiling::Auto.ceiling_mib(p.map(|g| g * GIB), a.map(|g| g * GIB))
        };
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
        assert_eq!(auto(Some(16), Some(0)), Some(4 * GIB));
        assert_eq!(auto(Some(16), Some(8)), Some(6 * GIB));
        assert_eq!(
            auto(Some(8), Some(0)),
            Some(2 * GIB),
            "a quarter of a small Mac"
        );
        assert_eq!(auto(None, Some(10)), Some(16 * GIB));
        assert_eq!(auto(Some(0), None), Some(16 * GIB));
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

    /// In trouble at critical pressure whatever the swap, and at warn only while
    /// swapping at least [`THRASH_SWAP_KIB_S`]; not at warn with swap under the
    /// line or unmeasured, at normal pressure, nor at an unknown level.
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
    /// memory in use.
    #[test]
    fn the_dominant_consumer_is_the_largest_and_a_fifth_of_the_memory_in_use() {
        let alone = m(280, 128, 2, 121, Pressure::Warn, Some(89));
        assert!(alone.dominant(), "28 of 121 GB: 23%");
        let tied = AgentMemory {
            largest_other_mib: Some(28 * GIB),
            ..alone
        };
        assert!(tied.dominant(), "as large as the largest other");
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

    /// With memory to spare every footprint past the ceiling is a warning only;
    /// it is acted on only when the Mac is in trouble and the agent dominates.
    #[test]
    fn a_measure_past_the_ceiling_warns_and_acts_only_in_trouble_on_the_dominant_agent() {
        use Pressure::{Critical, Normal, Warn};
        let auto = MemoryCeiling::Auto;
        let runaway = judge(auto, &m(2150, 128, 60, 68, Normal, Some(0))).expect("over");
        assert!(!runaway.acts(), "a warning only: {runaway:?}");
        assert!(runaway.dominant);
        assert_eq!(runaway.ceiling_mib, 46 * GIB);
        let quiet = judge(auto, &m(2150, 128, 8, 120, Warn, Some(0))).expect("over");
        assert!(!quiet.acts(), "{quiet:?}");
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
        let crit = judge(auto, &m(388, 192, 4, 136, Critical, None)).expect("over");
        assert!(crit.acts(), "{crit:?}");
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
    /// not warn again each time it crosses the footprint.
    #[test]
    fn a_warning_episode_ends_an_eighth_under_the_ceiling() {
        let auto = MemoryCeiling::Auto;
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

    /// The verdict moves with the Mac's trouble, this agent's rank and a
    /// restarting host, never with the numbers alone.
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
    /// to cut at, whatever the strain.
    #[test]
    fn the_fact_is_read_whole_as_the_banners_head() {
        let over = MemoryOver {
            footprint_mib: 220_279,
            ceiling_mib: 48 * GIB,
            available_mib: Some(60 * GIB),
            strain: None,
            dominant: true,
        };
        let swapping = MemoryOver {
            strain: Some(Strain::Swapping { swap_kib_s: 60_000 }),
            ..over
        };
        let critical = MemoryOver {
            strain: Some(Strain::Critical),
            ..over
        };
        for f in [over, swapping, critical] {
            assert_eq!(aterm_phase::memory_banner_head(&f.fact()), f.fact());
        }
    }

    /// The warning's resume line is kept whole or not at all ([`never_cut`]):
    /// under a renderer's cap a Claude Code's line steps down to its
    /// permission mode, then to its conversation alone, then to no line and
    /// the restart-by-hand words.
    #[test]
    fn the_warnings_resume_line_is_kept_whole_or_not_at_all() {
        let over = MemoryOver {
            footprint_mib: 51_302,
            ceiling_mib: 24 * GIB,
            available_mib: Some(12_595),
            strain: None,
            dominant: true,
        };
        let session = "0badf00d-1111-2222-3333-444455556666";
        let argv: Vec<String> = [
            "claude",
            "--model",
            "claude-opus-5-5",
            "--add-dir",
            "/Users//someone/a-rather-long-project-directory",
            "--permission-mode",
            "plan",
        ]
        .map(String::from)
        .to_vec();
        let line = crate::harness::resume::command(&argv, session).unwrap();
        let text = over.warning("Claude", false, Some(&line));
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
        let mode = format!(
            "{head}{}",
            crate::harness::resume::mode_of(&line).expect("a permission mode")
        );
        let bare = format!("{head}claude --resume {session}");
        assert!(
            mode.contains("plan") && !mode.contains("--add-dir"),
            "{mode}"
        );
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
        .warning("Claude", true, Some(&line));
        assert_eq!(never_cut(&restart, cap(400)), restart, "no line named");
        assert!(!cap(len(&text) - 1)(&text).contains(session));
    }

    /// The key's text round-trips, takes a unit, and reads zero as off.
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
    /// the same as itself.
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
