// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn sample() -> HandlerTally {
    let mut t = HandlerTally {
        pid: 4242,
        at_ns: 900_000_000,
        ..HandlerTally::default()
    };
    t.rows[HandlerRow::Deliver.index()].add(120_000, Unit::Ns);
    t.rows[HandlerRow::Deliver.index()].add(40_000_000, Unit::Ns);
    t.rows[HandlerRow::Post.index()].add(9_000, Unit::Ns);
    t.rows[HandlerRow::AwaitWake.index()].add(31_000, Unit::Ns);
    t
}

#[test]
fn a_handler_line_round_trips_at_one_length() {
    let t = sample();
    let line = t.render();
    assert!(
        line.starts_with("handler-tally v=2 pid=0000004242 at="),
        "{line}"
    );
    assert_eq!(HandlerTally::parse(&line), Ok(t));
    let zero = HandlerTally::default();
    let full = HandlerTally {
        pid: u32::MAX,
        at_ns: u64::MAX,
        rows: [Hist {
            count: u64::MAX,
            total: u64::MAX,
            max: u64::MAX,
            buckets: [u32::MAX; BUCKETS],
        }; HANDLER_ROWS],
    };
    assert_eq!(zero.render().len(), full.render().len());
    assert_eq!(zero.render().len(), line.len());
    assert_eq!(HandlerTally::parse(&full.render()), Ok(full));
    assert_eq!(HandlerTally::parse(&zero.render()), Ok(zero));
}

#[test]
fn a_torn_or_foreign_handler_line_is_refused() {
    let old = sample().render();
    let mut later = sample();
    later.rows[HandlerRow::Sent.index()].add(300_000, Unit::Ns);
    later.at_ns += 1;
    let new = later.render();
    assert_eq!(old.len(), new.len());
    for cut in 1..old.len() {
        let torn = format!("{}{}", &new[..cut], &old[cut..]);
        if torn != old && torn != new {
            assert!(HandlerTally::parse(&torn).is_err(), "cut {cut}");
        }
    }
    for bad in [
        "",
        "handler-tally v=2\n",
        &old[..old.len() - 1],
        &old.replace("handler-tally v=2", "handler-tally v=1"),
        &old.replace("handler-tally v=2", "wait-tally v=2"),
    ] {
        assert!(HandlerTally::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_difference_of_two_handler_lines_is_one_processs() {
    let earlier = sample();
    let mut later = earlier;
    later.at_ns += 5;
    later.rows[HandlerRow::Post.index()].add(1_000, Unit::Ns);
    let d = later.since(&earlier).expect("one process");
    assert_eq!(d.at_ns, 5);
    assert_eq!(d.row(HandlerRow::Post).count, 1);
    assert_eq!(d.row(HandlerRow::Post).total, 1_000);
    assert_eq!(d.row(HandlerRow::Deliver).count, 0);
    let other = HandlerTally { pid: 1, ..later };
    assert!(other.since(&earlier).is_err());
    assert!(earlier.since(&later).is_err(), "backwards");
}

#[test]
fn every_row_has_its_own_name_and_place() {
    for (i, row) in HandlerRow::ALL.into_iter().enumerate() {
        assert_eq!(row.index(), i);
    }
    let mut names: Vec<&str> = HandlerRow::ALL.iter().map(|r| r.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), HANDLER_ROWS);
}
