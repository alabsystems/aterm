// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// EVERY MODE READS BACK FROM ITS WORD, and from its other names, trimmed and in any case; no
/// other word reads as a mode.
#[test]
fn every_autopilot_mode_reads_back_from_its_words() {
    for mode in Autopilot::ALL {
        assert_eq!(Autopilot::parse(mode.word()), Some(mode), "{mode:?}");
    }
    for (word, want) in [
        ("cruise", Some(Autopilot::Cruise)),
        ("  CRUISE ", Some(Autopilot::Cruise)),
        ("forever", Some(Autopilot::Cruise)),
        ("Infinity", Some(Autopilot::Cruise)),
        ("land", Some(Autopilot::Land)),
        ("wrap-up", Some(Autopilot::Land)),
        ("WrapUp", Some(Autopilot::Land)),
        ("wrap", Some(Autopilot::Land)),
        ("off", Some(Autopilot::Off)),
        ("Off", Some(Autopilot::Off)),
        ("on", None),
        ("true", None),
        ("false", None),
        ("none", None),
        ("wrap it up", None),
        ("", None),
    ] {
        assert_eq!(Autopilot::parse(word), want, "{word:?}");
    }
}

/// A WALL'S RESET IS PLACED SOONER UNDER A SHORTER MINUTE, NEVER BEFORE ITS SCALED TIME: at a
/// twentieth, `in 1m` falls three seconds after a read on a whole second, and on the first whole
/// second at or after its scaled time from any other read; a timing whose reset sits where its
/// words name it, a reset at or before the read and one too far off to scale leave it there.
#[test]
fn a_walls_reset_is_placed_sooner_and_never_before_its_scaled_time() {
    let shipped = TurnEndTiming::default();
    let span = shipped.clone().reset_span_scaled(0.05);
    assert_eq!(span.reset_minute, Duration::from_secs(3));
    assert!(shipped.reset_as_named() && !span.reset_as_named());
    let read_s: i64 = 1_793_527_588;
    assert_eq!(span.reset_placed(read_s + 60, read_s * 1000), read_s + 3);
    // Read 700 ms into its second, `in 1m` is 59.3 s off: 2.965 s at a twentieth, placed on the
    // next whole second.
    let read_ms = read_s * 1000 + 700;
    assert_eq!(span.reset_placed(read_s + 60, read_ms), read_s + 4);
    for frac in [0, 1, 299, 500, 999] {
        for minutes in [1, 2, 5, 60] {
            let now_ms = read_s * 1000 + frac;
            let at = read_s + minutes * 60;
            let placed = span.reset_placed(at, now_ms);
            // Twenty times the scaled time, in milliseconds: the read's and the span's.
            let scaled = now_ms * 20 + (at * 1000 - now_ms);
            let after = placed * 1000 * 20 - scaled;
            assert!(
                (0..20 * 1000).contains(&after),
                "{minutes}m read {frac} ms into its second: placed {placed}, {after} past"
            );
            assert!(placed <= at, "{minutes}m: placed after its words' reset");
        }
    }
    assert_eq!(span.reset_placed(read_s, read_ms), read_s, "passed");
    assert_eq!(span.reset_placed(read_s - 60, read_ms), read_s - 60);
    assert_eq!(shipped.reset_placed(read_s + 60, read_ms), read_s + 60);
    let unbounded = TurnEndTiming {
        reset_minute: Duration::MAX,
        ..shipped
    };
    assert_eq!(unbounded.reset_placed(read_s + 60, read_ms), read_s + 60);
}
