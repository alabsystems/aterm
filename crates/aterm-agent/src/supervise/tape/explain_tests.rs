// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn at_reads_each_way_to_name_a_point() {
    let time = |day: Option<i64>, secs: u32| At::Time { day, secs };
    assert_eq!(At::parse("seq=102"), Ok(At::Seq(102)));
    assert_eq!(At::parse("door=17"), Ok(At::Door(17)));
    assert_eq!(At::parse("door=#17"), Ok(At::Door(17)));
    assert_eq!(
        At::parse("14:05:09"),
        Ok(time(None, 14 * 3600 + 5 * 60 + 9))
    );
    assert_eq!(At::parse("14:05"), Ok(time(None, 14 * 3600 + 5 * 60)));
    assert_eq!(
        At::parse("2026-10-20T14:05"),
        Ok(time(Some(20_746), 14 * 3600 + 5 * 60))
    );
    assert_eq!(
        At::parse("2026-10-20 14:05:09"),
        Ok(time(Some(20_746), 14 * 3600 + 5 * 60 + 9))
    );
    assert_eq!(
        At::parse("APPROVED seq=102 git log"),
        Ok(At::Line("APPROVED seq=102 git log".to_string()))
    );
    for line in [
        "25:00",
        "2026-02-30T10:00",
        "2026-10-20T24:00",
        "26-10-20T10:00",
    ] {
        assert_eq!(At::parse(line), Ok(At::Line(line.to_string())), "{line}");
    }
    assert!(At::parse("seq=x").is_err());
    assert!(At::parse("door=").is_err());
    assert!(At::parse("  ").is_err());
}
