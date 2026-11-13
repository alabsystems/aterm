// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_hint_round_trips_with_and_without_a_zoom() {
    let plain = WarmHint {
        windows: 3,
        sessions: 6,
        cols: 100,
        rows: 30,
        zoom: None,
        size_zoom: None,
    };
    assert_eq!(plain.encode(), "w1;windows=3;sessions=6;cols=100;rows=30");
    assert_eq!(WarmHint::parse(&plain.encode()), Some(plain));
    let zoomed = WarmHint {
        zoom: Some((28_000, 24_000)),
        size_zoom: Some((14_000, 12_000)),
        ..plain
    };
    assert_eq!(WarmHint::parse(&zoomed.encode()), Some(zoomed));
    assert_eq!(
        zoomed.carried_zoom(),
        crate::app_config::CarriedZoom {
            size: Some((14_000, 12_000)),
            device: Some((28_000, 24_000)),
        }
    );
    assert_eq!(
        plain.carried_zoom(),
        crate::app_config::CarriedZoom::default()
    );
    let older = WarmHint::parse("w1;windows=3;sessions=6;cols=100;rows=30;px=28000;reset=24000")
        .expect("an older hint reads");
    assert_eq!(older.size_zoom, None);
    assert_eq!(older.zoom, Some((28_000, 24_000)));
}

#[test]
fn anything_else_reads_as_no_hint() {
    for raw in [
        "",
        "w2;windows=1;sessions=1;cols=80;rows=24",
        "windows=1;sessions=1;cols=80;rows=24",
        "w1;windows=1;sessions=1;cols=80",
        "w1;windows=1;sessions=1;cols=80;rows=24;rows=25",
        "w1;windows=-1;sessions=1;cols=80;rows=24",
        "w1;windows=+1;sessions=1;cols=80;rows=24",
        "w1;windows=01;sessions=1;cols=80;rows=24",
        "w1;windows=00;sessions=1;cols=80;rows=24",
        "w1;windows=1;sessions=1;cols=80;rows=24;px=+28000;reset=24000",
        "w1;windows=1;sessions=1;cols=80;rows=24;px=028000;reset=24000",
        "w1;windows=1;sessions=1;cols=70000;rows=24",
        "w1;windows=1;sessions=1;cols=80;rows=24;px",
        "w1;windows=1;sessions=1;cols=80;rows=24;px=abc;reset=1",
    ] {
        assert_eq!(WarmHint::parse(raw), None, "{raw:?}");
    }
    let long = format!(
        "w1;windows=1;sessions=1;cols=80;rows=24;pad={}",
        "9".repeat(300)
    );
    assert_eq!(WarmHint::parse(&long), None, "oversized");
    assert_eq!(
        WarmHint::parse("w1;windows=1;sessions=2;cols=80;rows=24;px=20000;glyphs=x")
            .map(|hint| hint.zoom),
        Some(None)
    );
}
