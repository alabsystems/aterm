// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CRITICAL, NO_SYSTEM, NORMAL, Source, WARN, level_line, level_of, mask_of, resolve,
    scrollback_shed, source,
};
use crate::crash_restore::PressureLevel;

#[test]
fn the_seam_picks_the_source() {
    let seam = |v: &str| source(Some(v.into()));
    assert_eq!(source(None), Source::Kernel);
    assert_eq!(seam("critical"), Source::StandIn(PressureLevel::Critical));
    assert_eq!(seam("warn"), Source::StandIn(PressureLevel::Warn));
    assert_eq!(seam("normal"), Source::StandIn(PressureLevel::Normal));
    assert_eq!(
        seam(NO_SYSTEM),
        Source::Unheard,
        "a live test's instance hears no system"
    );
    assert_eq!(seam(""), Source::Kernel);
    assert_eq!(seam("swapping"), Source::Kernel, "a word that is no level");
    for level in [
        PressureLevel::Normal,
        PressureLevel::Warn,
        PressureLevel::Critical,
    ] {
        assert_eq!(
            level_of(mask_of(level)),
            Some(level),
            "the stand-in reports the bit the kernel would"
        );
    }
}

#[test]
fn the_level_line_names_the_level_the_census_reads() {
    let line = |mask| level_line(mask, level_of(mask));
    assert!(line(CRITICAL).contains("reports critical"));
    assert!(line(WARN | CRITICAL).contains("reports critical"));
    assert!(line(WARN).contains("reports warn"));
    assert!(line(NORMAL).contains("reports normal"));
    assert!(
        level_line(WARN | NORMAL, Some(PressureLevel::Normal))
            .contains("reports normal (level 0x3)"),
        "a merged mask's line says where the pressure stands, and what was delivered"
    );
    for level in [NORMAL, WARN, CRITICAL] {
        assert!(line(level).starts_with(crate::recovery_census::PRESSURE_LINE));
    }
}

#[test]
fn a_return_to_normal_is_recorded_and_never_sheds_the_scrollback() {
    assert_eq!(level_of(NORMAL), Some(PressureLevel::Normal));
    assert_eq!(level_of(0), None);
    assert_eq!(level_of(WARN), Some(PressureLevel::Warn));
    assert_eq!(level_of(CRITICAL), Some(PressureLevel::Critical));
    assert_eq!(level_of(WARN | CRITICAL), Some(PressureLevel::Critical));
    assert_eq!(scrollback_shed(PressureLevel::Normal), None);
    assert_eq!(scrollback_shed(PressureLevel::Warn), Some(false));
    assert_eq!(scrollback_shed(PressureLevel::Critical), Some(true));
}

#[test]
fn a_merged_mask_is_read_as_the_level_the_system_is_at_now() {
    let now = |level| move || Some(level);
    assert_eq!(
        resolve(WARN | NORMAL, now(PressureLevel::Normal)),
        Some(PressureLevel::Normal),
        "the spike eased"
    );
    assert_eq!(
        level_of(WARN | NORMAL),
        Some(PressureLevel::Warn),
        "control: the old reading kept the rings shed"
    );
    assert_eq!(
        resolve(NORMAL | WARN | CRITICAL, now(PressureLevel::Warn)),
        Some(PressureLevel::Warn)
    );
    assert_eq!(
        resolve(WARN | NORMAL, || None),
        Some(PressureLevel::Warn),
        "an unread level: the highest bit"
    );
    let unread = || -> Option<PressureLevel> { panic!("a single level is never read") };
    assert_eq!(resolve(CRITICAL, unread), Some(PressureLevel::Critical));
    assert_eq!(resolve(NORMAL, unread), Some(PressureLevel::Normal));
    assert_eq!(resolve(0, unread), None);
    assert_eq!(
        resolve(WARN | 0x10, unread),
        Some(PressureLevel::Warn),
        "a bit that is no level is not one"
    );
}
