// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const GIB: u64 = 1024;

/// The automatic ceiling follows [`MemoryCeiling::ceiling_mib`]. Negative
/// control: more available means a higher ceiling; an owner's number does
/// not move.
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

/// THE AUTOMATIC CEILING READS THE MEMORY `sys mem` CALLS AVAILABLE
/// (`aterm_sysprobe::MemoryDetail::available`, as the host's `Acts::memory` hands it on): at
/// the look on a 24 GiB Mac of 2026-10-29 (0.09 GiB free, 9.92 GiB inactive of which 2.62
/// file-backed, nothing purgeable; 6.9 GiB available by the reader's rule) the ceiling is an
/// eighth of the Mac, 3 GiB, and half of 6.9 GiB. NEGATIVE CONTROL: free plus every inactive
/// page plus purgeable, what the ceiling read until 2026-11-06, reads 10.0 GiB and a ceiling
/// of 8.0 GiB.
#[test]
fn the_automatic_ceiling_reads_the_machines_available_memory() {
    const MIB: u64 = 1 << 20;
    let hundredths = |h: u64| h * GIB * MIB / 100;
    let d = aterm_sysprobe::MemoryDetail {
        total: 24 * GIB * MIB,
        available: hundredths(690),
        free: hundredths(9),
        inactive: Some(hundredths(992)),
        file_backed: hundredths(262),
        purgeable: Some(0),
        ..aterm_sysprobe::MemoryDetail::default()
    };
    let available = d.available / MIB;
    let physical = Some(d.total / MIB);
    let ceiling = MemoryCeiling::Auto.ceiling_mib(physical, Some(available));
    assert_eq!(ceiling, Some(3 * GIB + available / 2));
    assert_eq!(
        ceiling.map(|c| c * 10 / GIB),
        Some(64),
        "6.4 GiB: {ceiling:?}"
    );
    let every_inactive = (d.free + d.inactive.unwrap_or(0) + d.purgeable.unwrap_or(0)) / MIB;
    let before = MemoryCeiling::Auto.ceiling_mib(physical, Some(every_inactive));
    assert_eq!(
        before.map(|c| c * 10 / GIB),
        Some(80),
        "8.0 GiB: {before:?}"
    );
    assert_ne!(ceiling, before);
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
