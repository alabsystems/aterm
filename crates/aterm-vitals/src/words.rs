// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The words every surface prints: the three scope sentences (design "At a
//! glance"), and the compact figures the Deck draws beside its bars. One
//! function per figure, so the Deck, accessibility, `aterm ctl sys` and an
//! agent read the same text.
//!
//! Units: memory in binary GiB/MiB (what Activity Monitor shows), disk in
//! decimal TB/GB (what Finder shows), CPU in cores-worth (`2.3c`: 2.3 cores
//! busy for the interval). An unread figure is left out, never printed as 0.

use crate::model::{Snapshot, TabView};
use crate::trend::Motion;

/// The noise gate for a cores-worth figure's motion: a tenth of a core.
pub const CORES_GATE: f32 = 0.1;
/// The noise gate for a footprint's motion, GiB.
pub const FOOTPRINT_GATE_GIB: f32 = 0.05;
/// The noise gate for one core's or the GPU's busy fraction: 4 points.
pub const LOAD_GATE: f32 = 0.04;

/// Cores-worth to one decimal: `3.1`.
#[must_use]
pub fn cores(c: f32) -> String {
    format!("{:.1}", c.max(0.0))
}

/// Bytes in binary units for memory: `310 MiB`, `2.3 GiB`, `17.7 GiB`.
#[must_use]
pub fn mem(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.0} MiB", b / MIB)
    } else {
        format!("{:.0} KiB", b / 1024.0)
    }
}

/// GiB to one decimal without the unit: `17.7`.
#[must_use]
pub fn gib_figure(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / f64::from(1u32 << 30))
}

/// Physical memory as people name it: `32 GiB` (whole when it is whole).
#[must_use]
pub fn gib_total(bytes: u64) -> String {
    let g = bytes as f64 / f64::from(1u32 << 30);
    if (g - g.round()).abs() < 0.05 {
        format!("{g:.0}")
    } else {
        format!("{g:.1}")
    }
}

/// Bytes in decimal units for disk: `1.81 TB`, `412 GB`, `9.4 GB`.
#[must_use]
pub fn disk(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e12 {
        format!("{:.2} TB", b / 1e12)
    } else if b >= 100e9 {
        format!("{:.0} GB", b / 1e9)
    } else if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else {
        format!("{:.0} MB", b / 1e6)
    }
}

/// `rising` / `easing` after a figure, or nothing while steady.
fn motion_suffix(m: Motion) -> String {
    m.word().map(|w| format!(", {w}")).unwrap_or_default()
}

/// The Global sentence: `Global: 3.1 of 10 cores busy, rising; GPU 4%;
/// memory 17.7 of 32 GiB used, pressure normal; swap 0; disk 1.81 TB free.`
#[must_use]
pub fn global_sentence(s: &Snapshot) -> String {
    let mut parts = Vec::new();
    if let Some(cpu) = s.cpu.as_ref() {
        parts.push(format!(
            "{} of {} cores busy{}",
            cores(cpu.busy.level),
            cpu.total(),
            motion_suffix(cpu.busy.motion(CORES_GATE))
        ));
    }
    if let Some(g) = s.gpu.as_ref() {
        parts.push(format!("GPU {:.0}%", (g.load.raw * 100.0).round()));
    }
    if let Some(m) = s.mem.as_ref() {
        let used = m.sample.used();
        let mut w = format!(
            "memory {} of {} GiB used",
            gib_figure(used),
            gib_total(m.sample.total)
        );
        if m.sample.pressure != crate::model::Pressure::Unknown {
            w.push_str(&format!(", pressure {}", m.sample.pressure.word()));
        }
        parts.push(w);
        if m.sample.swap_used == 0 {
            parts.push("swap 0".into());
        } else {
            parts.push(format!("swap {}", mem(m.sample.swap_used)));
        }
    }
    if let Some(v) = s.volume.as_ref() {
        parts.push(format!("disk {} free", disk(v.available)));
    }
    if parts.is_empty() {
        return "Global: not read yet.".into();
    }
    format!("Global: {}.", parts.join("; "))
}

/// The aterm sentence: `aterm: 1.6 cores, 52% of busy; 2.3 GiB, rising; 17
/// processes.`
#[must_use]
pub fn aterm_sentence(s: &Snapshot) -> String {
    let Some(a) = s.aterm.as_ref() else {
        return "aterm: measuring.".into();
    };
    let mut first = format!("{} cores", cores(a.cores.level));
    if let Some(busy) = s.cpu.as_ref().map(|c| c.busy.level).filter(|b| *b > 0.05) {
        let share = (a.cores.level / busy * 100.0).round().clamp(0.0, 100.0);
        first.push_str(&format!(", {share:.0}% of busy"));
    }
    let mut parts = vec![first];
    if let Some(f) = a.footprint {
        parts.push(format!(
            "{}{}",
            mem(f),
            motion_suffix(a.footprint_gib.motion(FOOTPRINT_GATE_GIB))
        ));
    }
    parts.push(format!(
        "{} process{}",
        a.procs,
        if a.procs == 1 { "" } else { "es" }
    ));
    format!("aterm: {}.", parts.join("; "))
}

/// A tab's sentence: `Tab 2: claude; 0.9 cores; 310 MiB.`
#[must_use]
pub fn tab_sentence(t: &TabView) -> String {
    let mut parts = Vec::new();
    if !t.program.is_empty() {
        parts.push(t.program.clone());
    }
    parts.push(format!(
        "{} cores{}",
        cores(t.scope.cores.level),
        motion_suffix(t.scope.cores.motion(CORES_GATE))
    ));
    if let Some(f) = t.scope.footprint {
        parts.push(mem(f));
    }
    if let Some((name, c)) = t.scope.top.as_ref().filter(|(_, c)| *c >= 0.1)
        && *name != t.program
    {
        parts.push(format!("busiest {name} {}c", cores(*c)));
    }
    format!("{}: {}.", t.label, parts.join("; "))
}

/// The compact CPU figure the Deck draws: `3.1c/10▴`.
#[must_use]
pub fn cores_figure(level: f32, total: Option<usize>, motion: Motion) -> String {
    let mut s = format!("{}c", cores(level));
    if let Some(n) = total {
        s.push_str(&format!("/{n}"));
    }
    if let Some(g) = motion.glyph() {
        s.push(g);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CpuView, GpuView, MemSample, MemView, Pressure, ScopeView, TabView, VolumeSample,
    };
    use crate::trend::TrendView;

    fn scenario_a() -> Snapshot {
        Snapshot {
            cpu: Some(CpuView {
                cores: vec![crate::model::CoreView::default(); 10],
                busy: TrendView::flat(3.1),
                ..CpuView::default()
            }),
            gpu: Some(GpuView {
                load: TrendView::flat(0.04),
                ..GpuView::default()
            }),
            mem: Some(MemView {
                sample: MemSample {
                    total: 32 << 30,
                    app: 12 << 30,
                    wired: (4 << 30) + (700 << 20),
                    compressed: 1 << 30,
                    pressure: Pressure::Normal,
                    ..MemSample::default()
                },
                ..MemView::default()
            }),
            volume: Some(VolumeSample {
                total: 2_000_000_000_000,
                free: 1_810_000_000_000,
                available: 1_810_000_000_000,
                mount: "/".into(),
            }),
            aterm: Some(ScopeView {
                cores: TrendView::flat(1.6),
                footprint: Some((2 << 30) + (300 << 20)),
                procs: 17,
                ..ScopeView::default()
            }),
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_global_sentence_reads_like_the_design() {
        assert_eq!(
            global_sentence(&scenario_a()),
            "Global: 3.1 of 10 cores busy; GPU 4%; memory 17.7 of 32 GiB used, pressure normal; \
             swap 0; disk 1.81 TB free."
        );
    }

    #[test]
    fn a_rising_figure_says_so() {
        let mut s = scenario_a();
        s.cpu.as_mut().unwrap().busy = TrendView {
            raw: 4.0,
            level: 3.1,
            slow: 2.0,
            mad: 0.0,
        };
        assert!(global_sentence(&s).contains("3.1 of 10 cores busy, rising;"));
    }

    #[test]
    fn the_aterm_sentence_gives_its_share_of_busy() {
        assert_eq!(
            aterm_sentence(&scenario_a()),
            "aterm: 1.6 cores, 52% of busy; 2.3 GiB; 17 processes."
        );
        assert_eq!(aterm_sentence(&Snapshot::default()), "aterm: measuring.");
    }

    #[test]
    fn a_tab_sentence_names_its_program_and_busiest_helper() {
        let t = TabView {
            key: 1,
            label: "Tab 2".into(),
            program: "claude".into(),
            scope: ScopeView {
                cores: TrendView::flat(0.9),
                footprint: Some(310 << 20),
                top: Some(("cargo".into(), 0.7)),
                ..ScopeView::default()
            },
        };
        assert_eq!(
            tab_sentence(&t),
            "Tab 2: claude; 0.9 cores; 310 MiB; busiest cargo 0.7c."
        );
    }

    #[test]
    fn an_unread_figure_is_left_out_never_zero() {
        assert_eq!(
            global_sentence(&Snapshot::default()),
            "Global: not read yet."
        );
        let mut s = scenario_a();
        s.gpu = None;
        s.volume = None;
        let w = global_sentence(&s);
        assert!(!w.contains("GPU") && !w.contains("disk"), "{w}");
    }

    #[test]
    fn units_follow_the_platform_conventions() {
        assert_eq!(mem(310 << 20), "310 MiB");
        assert_eq!(mem(2_469_606_195), "2.3 GiB");
        assert_eq!(disk(1_810_000_000_000), "1.81 TB");
        assert_eq!(disk(412_000_000_000), "412 GB");
        assert_eq!(disk(9_400_000_000), "9.4 GB");
        assert_eq!(gib_total(32 << 30), "32");
        assert_eq!(cores(-0.2), "0.0");
        assert_eq!(
            cores_figure(3.06, Some(10), Motion::Rising),
            "3.1c/10\u{25B4}"
        );
        assert_eq!(cores_figure(0.9, None, Motion::Steady), "0.9c");
    }
}
