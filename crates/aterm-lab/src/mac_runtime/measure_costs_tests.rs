// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn concurrent_wall_is_a_union_and_an_unfinished_row_is_not_zero() {
    assert_eq!(union_ms(vec![(10, 30), (10, 20), (15, 25), (40, 50)]), 30);
    assert_ne!(
        union_ms(vec![(10, 30), (10, 20)]),
        30,
        "negative control: overlapping row walls must not be added"
    );
    let boundary = |event, row, at, seconds| {
        format!(
            "verify-event: {}\n",
            json!({"event":event,"matrix":"test","row":row,"at_unix_ms":at,"seconds":seconds})
        )
    };
    let mut log = "measure kind: test::a Count { windowed: false }\nmeasure kind: test::b Count { windowed: true }\n".to_owned();
    log += &boundary("measure-row-started", "a", 1000, 0.0);
    log += &boundary("measure-row-started", "b", 1000, 0.0);
    log += &boundary("measure-row-finished", "a", 11000, 10.0);
    assert!(read(&log).is_err());
    log += &boundary("measure-row-finished", "b", 21000, 20.0);
    log += &boundary("measure-row-started", "solo", 21000, 0.0);
    log += &boundary("measure-row-finished", "solo", 26000, 5.0);
    let costs = read(&log).unwrap();
    assert_eq!(costs["count_phase_s"]["test"].as_f64(), Some(20.0));
    assert_eq!(costs["timing_rows_s"].as_f64(), Some(5.0));
    assert_eq!(costs["rows"]["test::solo"]["kind"].as_str(), Some("Timing"));
    log += "measure setup: paint scene=Prompt capture_s=2 pacing_s=1\nmeasure setup: paint scene=Plane capture_s=3 pacing_s=1\n";
    assert_eq!(
        read(&log).unwrap()["paint_setup_totals_s"]["capture_s"].as_f64(),
        Some(5.)
    );
    assert_eq!(
        read(&log).unwrap()["paint_setup_totals_s"]["pacing_s"].as_f64(),
        Some(2.)
    );
    assert!(read(&log.replace("capture_s=3", "capture_s=NaN")).is_err());
    assert!(read(&log.replace("capture_s=3 pacing_s=1", "capture_s=3")).is_err());
    assert!(read(&log.replace("capture_s=3", "capture_s=3 capture_s=3")).is_err());
    assert!(
        read(
            &log.replace("capture_s=2", "capture_s=1e308")
                .replace("capture_s=3", "capture_s=1e308")
        )
        .is_err()
    );
    log += "measure phase: row=busy_rss record=false phase=ready seconds=9.5\n";
    assert_eq!(
        read(&log).unwrap()["setup_phases"]["row=busy_rss record=false"]["ready"].as_f64(),
        Some(9.5)
    );
    assert!(read(&log.replace("seconds=9.5", "seconds=NaN")).is_err());
    log += "measure phase: row=busy_rss record=false phase=ready seconds=0.5\n";
    assert!(
        read(&log).is_err(),
        "a duplicate phase cannot overwrite the first span"
    );
}
