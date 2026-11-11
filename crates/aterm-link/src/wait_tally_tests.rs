// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::wait_tally::checksum;

fn hist(values: &[u64], unit: Unit) -> Hist {
    let mut h = Hist::default();
    for &v in values {
        h.add(v, unit);
    }
    h
}

fn sample() -> Tally {
    let mut t = Tally {
        pid: 4242,
        committed: 119,
        at_ns: 900_000_000,
        dir: true,
        flushes: astream_sys::FlushTally {
            full: 3,
            barrier: 40,
            plain: 5,
        },
        ..Tally::default()
    };
    t.rows[Row::Request.index()] = hist(&[31_000_000], Unit::Ns);
    t.rows[Row::Ack.index()] = hist(&[9_000, 18_000_000], Unit::Ns);
    t.rows[Row::Commit.index()] = hist(&[9_000], Unit::Ns);
    t.rows[Row::LaneDeliver.index()] = hist(&[120_000, 250_000, 40_000_000], Unit::Ns);
    t.rows[Row::DirReplace.index()] = hist(&[200_000], Unit::Ns);
    t.rows[Row::DrainPosts.index()] = hist(&[0, 1, 1, 3], Unit::Count);
    t.rows[Row::Park.index()] = hist(&[500_000_000], Unit::Ns);
    // Its three calls past 1.05 ms, in the order they ended; the park is no call.
    for c in [
        call(Row::Request, 7_000_000_000, 31_000_000),
        call(Row::Ack, 7_020_000_000, 18_000_000),
        call(Row::LaneDeliver, 7_100_000_000, 40_000_000),
    ] {
        t.calls.push(c);
    }
    t.times = Flushed {
        full_ns: 12_500_000,
        barrier_ns: 3_000_000,
        plain_ns: 0,
        slow_count: 2,
        running: 1,
        ..Flushed::default()
    };
    t.times.slow[0] = Some(slow(
        0,
        astream_sys::FlushKind::Full,
        7_000_002_000,
        7_009_000_000,
    ));
    t.times.slow[1] = Some(slow(
        1,
        astream_sys::FlushKind::Barrier,
        7_100_000_000,
        7_101_500_000,
    ));
    t
}

fn slow(
    index: u64,
    kind: astream_sys::FlushKind,
    start_ns: u64,
    end_ns: u64,
) -> astream_sys::SlowFlush {
    astream_sys::SlowFlush {
        index,
        kind,
        start_ns,
        end_ns,
    }
}

const fn call(row: Row, start_ns: u64, ns: u64) -> SlowCall {
    SlowCall { row, start_ns, ns }
}

/// THE BANDS ARE LOG2, and the edges are where the unit says: under a
/// microsecond-ish 1024 ns is band 0, 1024 opens band 1, 2^26 ns (67.1 ms)
/// and anything past it is the last; a count's band 0 is 0 alone.
#[test]
fn a_value_falls_in_its_log2_band_and_each_band_starts_at_its_floor() {
    assert_eq!(Hist::band(0, Unit::Ns), 0);
    assert_eq!(Hist::band(1023, Unit::Ns), 0);
    assert_eq!(Hist::band(1024, Unit::Ns), 1);
    assert_eq!(Hist::band(2047, Unit::Ns), 1);
    assert_eq!(Hist::band(2048, Unit::Ns), 2);
    assert_eq!(Hist::band((1 << 26) - 1, Unit::Ns), 16);
    assert_eq!(Hist::band(1 << 26, Unit::Ns), BUCKETS - 1);
    assert_eq!(Hist::band(u64::MAX, Unit::Ns), BUCKETS - 1);
    assert_eq!(Hist::band(0, Unit::Count), 0);
    assert_eq!(Hist::band(1, Unit::Count), 1);
    assert_eq!(Hist::band(3, Unit::Count), 2);
    assert_eq!(Hist::band(4, Unit::Count), 3);
    for unit in [Unit::Ns, Unit::Count] {
        for i in 1..BUCKETS {
            let floor = Hist::floor(i, unit);
            assert_eq!(Hist::band(floor, unit), i, "{unit:?} band {i}");
            assert_eq!(
                Hist::band(floor - 1, unit),
                i - 1,
                "{unit:?} below band {i}"
            );
        }
    }
    let h = hist(&[500, 3_000, 9_000_000, 70_000_000], Unit::Ns);
    assert_eq!((h.count, h.total, h.max), (4, 79_003_500, 70_000_000));
    assert_eq!(
        h.at_least(Hist::band(1 << 20, Unit::Ns)),
        2,
        "two at 1 ms or more"
    );
    assert_eq!(h.top(Unit::Ns), Some((1 << 26, None)), "the open last band");
    assert_eq!(
        hist(&[3_000], Unit::Ns).top(Unit::Ns),
        Some((2048, Some(4096)))
    );
    assert_eq!(Hist::default().top(Unit::Ns), None);
}

/// THE V=6 LINE READS BACK AS WRITTEN, AND EVERY LINE IS ONE LENGTH, so a
/// write at offset 0 always replaces the line whole: the smallest tally
/// and the largest render to the same width, a full ring of slow calls and
/// a full flush ring included.
#[test]
fn a_tally_line_reads_back_as_written_and_every_line_is_one_length() {
    let t = sample();
    let line = t.render();
    assert!(
        line.starts_with("wait-tally v=8 rows=031 pid=0000004242 "),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            " full=00000000000000000003 barrier=00000000000000000040 \
             plain=00000000000000000005 counted={FLUSHES_COUNTED} clock={} \
             full_ns=00000000000012500000 barrier_ns=00000000000003000000 \
             plain_ns=00000000000000000000 slow=00000000000000000002 \
             running=00000000000000000001 request=",
            FLUSH_CLOCK.word()
        )),
        "the flush counts, what they count, their clock and times end the head: {line}"
    );
    assert!(
        line.contains(&format!(
            " slow_calls=00000000000000000003 calls=00{:016x}+{:016x}.02{:016x}+{:016x}.\
             07{:016x}+{:016x}.--0000000000000000+0000000000000000.",
            7_000_000_000u64,
            31_000_000u64,
            7_020_000_000u64,
            18_000_000u64,
            7_100_000_000u64,
            40_000_000u64
        )) && line.contains(&format!(
            " ring=f{:016x}@{:016x}-{:016x}.b{:016x}@{:016x}-{:016x}.\
             -0000000000000000@0000000000000000-0000000000000000.",
            0u64, 7_000_002_000u64, 7_009_000_000u64, 1u64, 7_100_000_000u64, 7_101_500_000u64
        )),
        "the slow calls, then the flush ring, each oldest first, then its empty places: {line}"
    );
    assert_eq!(
        (t.times.slow().count(), t.calls.len()),
        (2, 3),
        "each ring's entries end at its first empty place"
    );
    assert_eq!(Tally::parse(&line), Ok(t));
    assert_eq!(t.blocked_ns(), 31_000_000 + 18_009_000);
    let zero = Tally::default();
    let full = Tally {
        pid: u32::MAX,
        committed: u64::MAX,
        at_ns: u64::MAX,
        dir: true,
        flushes: astream_sys::FlushTally {
            full: u64::MAX,
            barrier: u64::MAX,
            plain: u64::MAX,
        },
        times: Flushed {
            full_ns: u64::MAX,
            barrier_ns: u64::MAX,
            plain_ns: u64::MAX,
            slow_count: u64::MAX,
            slow: std::array::from_fn(|i| {
                let index = u64::MAX - RING as u64 + i as u64;
                Some(slow(
                    index,
                    astream_sys::FlushKind::Plain,
                    u64::MAX,
                    u64::MAX,
                ))
            }),
            running: u64::MAX,
        },
        rows: [Hist {
            count: u64::MAX,
            total: u64::MAX,
            max: u64::MAX,
            buckets: [u32::MAX; BUCKETS],
        }; ROWS],
        calls: SlowCalls {
            count: u64::MAX,
            kept: [Some(call(Row::TallyWrite, u64::MAX, u64::MAX)); CALLS_KEPT],
        },
    };
    assert_eq!(zero.render().len(), full.render().len());
    assert_eq!(
        [zero.render().len(), full.render().len(), line.len()],
        [line_len(); 3],
        "the counted length is every line's"
    );
    assert_eq!(Tally::parse(&full.render()), Ok(full));
    assert_eq!(full.blocked_ns(), u64::MAX, "saturates");
}

/// NEGATIVE CONTROL: a whole, checksummed line that names another row count is refused by
/// its count, never read with its rows shifted; so is one whose flush counts count other
/// flushes than this build's.
#[test]
fn a_line_of_another_row_count_or_flush_word_is_refused_by_its_head() {
    let line = sample().render();
    let (body, _) = line.trim_end().rsplit_once(" sum=").expect("a sum");
    let resummed = |body: String| format!("{body} sum={:016x}\n", checksum(body.as_bytes()));
    assert_eq!(
        Tally::parse(&resummed(body.to_string())),
        Ok(sample()),
        "the control: re-summed as it was, it reads"
    );
    let fewer = body.replacen("rows=031", "rows=025", 1);
    let why = Tally::parse(&resummed(fewer)).expect_err("another row count");
    assert!(
        why.contains("of 25 rows") && why.contains("reads 31"),
        "{why}"
    );
    let other = if FLUSHES_COUNTED == "all" {
        "barrier"
    } else {
        "all"
    };
    let word = body.replacen(
        &format!(" counted={FLUSHES_COUNTED} "),
        &format!(" counted={other} "),
        1,
    );
    let why = Tally::parse(&resummed(word)).expect_err("another flush word");
    assert!(why.contains(&format!("count `{other}`")), "{why}");
    // Times on another build's clock do not line up with this build's: refused by the word.
    let other = match FLUSH_CLOCK {
        astream_sys::FlushClock::Machine => astream_sys::FlushClock::Process,
        astream_sys::FlushClock::Process => astream_sys::FlushClock::Machine,
    };
    let clock = body.replacen(
        &format!(" clock={} ", FLUSH_CLOCK.word()),
        &format!(" clock={} ", other.word()),
        1,
    );
    let why = Tally::parse(&resummed(clock)).expect_err("another clock");
    assert!(
        why.contains(&format!("the `{}` clock", other.word())),
        "{why}"
    );
}

/// NEGATIVE CONTROL: a checksummed line whose flush ring or slow-call ring is no writer's is
/// refused, never read as flushes or calls: a flush place of no kind, an empty place with a
/// span, a flush after an empty place, one that ends before it starts, a short or a long
/// ring; a call of a row that is no call's or of none, one under 1.05 ms, a call after an
/// empty place, one that ends before the call before it, a start that is not 16 hex digits,
/// a short or a long ring, and a count the kept calls do not match.
#[test]
fn a_flush_ring_or_a_call_ring_no_writer_wrote_is_refused() {
    let line = sample().render();
    let (body, _) = line.trim_end().rsplit_once(" sum=").expect("a sum");
    let resummed = |body: String| format!("{body} sum={:016x}\n", checksum(body.as_bytes()));
    let (head, ring) = body.rsplit_once(" ring=").expect("a ring");
    let places: Vec<&str> = ring.split('.').collect();
    assert_eq!(places.len(), RING);
    let with = |places: &[String]| resummed(format!("{head} ring={}", places.join(".")));
    let owned: Vec<String> = places.iter().map(|p| (*p).to_string()).collect();
    assert_eq!(Tally::parse(&with(&owned)), Ok(sample()), "the control");
    let mut each: Vec<(String, Vec<String>)> = Vec::new();
    let mut put = |why: &str, at: usize, place: String| {
        let mut p = owned.clone();
        p[at] = place;
        each.push((why.to_string(), p));
    };
    put("a place of no kind", 0, owned[0].replacen('f', "x", 1));
    put(
        "an empty place with a span",
        2,
        format!("-{:016x}@{:016x}-{:016x}", 0, 1, 2),
    );
    put("a flush after an empty place", 3, owned[0].clone());
    put(
        "an end before its start",
        0,
        format!("f{:016x}@{:016x}-{:016x}", 0, 9, 8),
    );
    put("a place with no end", 0, format!("f{:016x}@{:016x}", 0, 9));
    put(
        "a place with no index (a v=6 line's)",
        0,
        format!("f{:016x}-{:016x}", 1, 2),
    );
    let second = |index: u64| {
        format!(
            "b{index:016x}@{:016x}-{:016x}",
            7_100_000_000u64, 7_101_500_000u64
        )
    };
    assert_eq!(second(1), owned[1], "the control's second place");
    put("an index not past the one before it", 1, second(0));
    put("an index at the line's slow count", 1, second(2));
    let mut short = owned.clone();
    short.pop();
    each.push(("a short ring".to_string(), short));
    let mut long = owned.clone();
    long.push(owned[RING - 1].clone());
    each.push(("a long ring".to_string(), long));
    for (why, places) in each {
        assert!(Tally::parse(&with(&places)).is_err(), "{why}");
    }

    let (before, rest) = body.split_once(" slow_calls=").expect("the slow calls");
    let (count, rest) = rest.split_once(" calls=").expect("the call ring");
    let (calls, after) = rest.split_once(" ring=").expect("the flush ring");
    let places: Vec<String> = calls.split('.').map(str::to_string).collect();
    assert_eq!((count, places.len()), ("00000000000000000003", CALLS_KEPT));
    let with = |count: &str, places: &[String]| {
        resummed(format!(
            "{before} slow_calls={count} calls={} ring={after}",
            places.join(".")
        ))
    };
    assert_eq!(
        Tally::parse(&with(count, &places)),
        Ok(sample()),
        "the control"
    );
    let place = |row: &str, start: u64, ns: u64| format!("{row}{start:016x}+{ns:016x}");
    let mut each: Vec<(&str, Vec<String>)> = Vec::new();
    let mut put = |why: &'static str, at: usize, p: String| {
        let mut all = places.clone();
        all[at] = p;
        each.push((why, all));
    };
    let park = format!("{:02}", Row::Park.index());
    put(
        "the park, no call",
        2,
        place(&park, 7_100_000_000, 40_000_000),
    );
    put(
        "a row past the last",
        2,
        place(&format!("{ROWS:02}"), 7_100_000_000, 40_000_000),
    );
    put(
        "a row not in digits",
        2,
        place("0x", 7_100_000_000, 40_000_000),
    );
    put(
        "a call under 1.05 ms",
        2,
        place("07", 7_100_000_000, (1 << 20) - 1),
    );
    put(
        "a call that ends before the one before it",
        2,
        place("07", 7_000_000_000, 20_000_000),
    );
    put("a call after an empty place", 4, places[2].clone());
    put("an empty place with a span", 3, place("--", 0, 1));
    put(
        "a start not in hex",
        2,
        format!("07{}+{:016x}", "00000001a2b3c4dz", 40_000_000),
    );
    put("a place with no length", 2, places[2].replace('+', ""));
    let mut short = places.clone();
    short.pop();
    each.push(("a short ring", short));
    let mut long = places.clone();
    long.push(places[CALLS_KEPT - 1].clone());
    each.push(("a long ring", long));
    for (why, all) in each {
        assert!(Tally::parse(&with(count, &all)).is_err(), "{why}");
    }
    for other in ["00000000000000000002", "00000000000000000004"] {
        assert!(
            Tally::parse(&with(other, &places)).is_err(),
            "a count of {other} with three kept"
        );
    }
}

/// NEGATIVE CONTROL: a line whose checksum was corrupted is refused, and so
/// is every mix of two lines a read racing the write could see — never
/// read as a count that is half old and half new.
#[test]
fn a_line_that_raced_its_write_or_lost_its_checksum_is_refused_never_read_as_a_count() {
    let old = sample().render();
    let mut later = sample();
    later.committed = 121;
    later.rows[Row::LaneDeliver.index()].add(300_000, Unit::Ns);
    later.rows[Row::Park.index()].add(1_000_000, Unit::Ns);
    let new = later.render();
    assert_eq!(old.len(), new.len());
    for cut in 1..old.len() {
        let torn = format!("{}{}", &new[..cut], &old[cut..]);
        if torn != old && torn != new {
            assert!(Tally::parse(&torn).is_err(), "cut {cut}");
        }
    }
    assert!(Tally::parse(&old).is_ok() && Tally::parse(&new).is_ok());
    let (body, sum) = old.trim_end().rsplit_once(" sum=").expect("a sum");
    let flipped: String = sum
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i == 0 {
                if c == '0' {
                    '1'
                } else {
                    '0'
                }
            } else {
                c
            }
        })
        .collect();
    let corrupt = format!("{body} sum={flipped}\n");
    assert!(
        Tally::parse(&corrupt)
            .expect_err("a corrupted checksum")
            .contains("checksum"),
        "{corrupt}"
    );
    for bad in [
        "",
        "wait-tally v=8\n",
        &old[..old.len() - 1],
        &old.replace("wait-tally v=8", "wait-tally v=7"),
        &old.replace(" sum=", " sum=0"),
    ] {
        assert!(Tally::parse(bad).is_err(), "{bad:?}");
    }
    for body in ["wait-tally v=8 rows=031 pid=1 requests=1".to_string(), {
        let (body, _) = old.trim_end().rsplit_once(" sum=").expect("a sum");
        body.rsplit_once('.').expect("a band").0.to_string()
    }] {
        let short = format!("{body} sum={:016x}\n", checksum(body.as_bytes()));
        assert!(Tally::parse(&short).is_err(), "{short}");
    }
}

#[test]
fn the_difference_of_two_tallies_is_one_processs_and_never_negative() {
    let earlier = sample();
    let mut later = earlier;
    later.committed = 200;
    later.at_ns += 50_000_000;
    later.rows[Row::LaneDeliver.index()].add(100_000, Unit::Ns);
    later.rows[Row::Ack.index()].add(90_000_000, Unit::Ns);
    later.flushes.barrier += 6;
    let d = later.since(&earlier).expect("one process");
    assert_eq!((d.pid, d.committed, d.at_ns), (4242, 200, 50_000_000));
    assert_eq!(
        d.flushes,
        astream_sys::FlushTally {
            full: 0,
            barrier: 6,
            plain: 0,
        },
        "the window's flushes"
    );
    assert_eq!(
        *d.row(Row::LaneDeliver),
        Hist {
            max: 0,
            ..hist(&[100_000], Unit::Ns)
        },
        "the window's one delivery, below the 40 ms before it: its largest not known"
    );
    assert_eq!(
        d.row(Row::Ack).max,
        90_000_000,
        "a new largest is the window's"
    );
    assert_eq!(d.row(Row::Request), &Hist::default(), "nothing new");
    let mut quieter = later;
    quieter.rows[Row::LaneDeliver.index()].add(130_000, Unit::Ns);
    let q = quieter.since(&later).expect("one process");
    assert_eq!(q.row(Row::LaneDeliver).max, 0);
    assert_eq!(
        q.row(Row::LaneDeliver).top(Unit::Ns),
        Some((131_072 / 2, Some(131_072)))
    );
    let relaunched = Tally { pid: 4243, ..later };
    assert!(relaunched
        .since(&earlier)
        .unwrap_err()
        .contains("relaunched"));
    let mut backwards = later;
    backwards.rows[Row::Ack.index()].count = 0;
    assert!(backwards.since(&earlier).unwrap_err().contains("`ack`"));
    let clock_back = Tally { at_ns: 1, ..later };
    assert!(clock_back.since(&earlier).unwrap_err().contains("clock"));
    let mut flushes_back = later;
    flushes_back.flushes.full = 0;
    assert!(flushes_back
        .since(&earlier)
        .unwrap_err()
        .contains("`full` flushes"));

    // EVERY SLOW CALL THE WINDOW MADE IS THE WINDOW'S, the slowest of its row or not: the
    // 90 ms ack above and a 2 ms delivery under the 40 ms one before the window are both
    // in the difference, and none of the calls the earlier write kept is. The flush times
    // subtract, the slow count with them, and the flush ring is the later write's whole.
    later.calls.push(call(Row::Ack, 7_400_000_000, 90_000_000));
    later.rows[Row::LaneDeliver.index()].add(2_000_000, Unit::Ns);
    later
        .calls
        .push(call(Row::LaneDeliver, 7_600_000_000, 2_000_000));
    later.times.full_ns += 9_000_000;
    later.times.slow_count += 1;
    later.times.running = 2;
    later.times.slow[2] = Some(slow(
        2,
        astream_sys::FlushKind::Full,
        7_500_000_100,
        7_509_000_000,
    ));
    let d = later.since(&earlier).expect("one process");
    assert_eq!(
        d.row(Row::LaneDeliver).max,
        0,
        "its largest not the window's"
    );
    assert_eq!(
        (d.calls.count, d.calls.iter().collect::<Vec<_>>()),
        (
            2,
            vec![
                call(Row::Ack, 7_400_000_000, 90_000_000),
                call(Row::LaneDeliver, 7_600_000_000, 2_000_000)
            ]
        ),
        "the window's slow calls, the one under its row's earlier largest included"
    );
    assert_eq!(
        d.calls.count,
        Row::ALL
            .iter()
            .filter(|r| r.is_call())
            .map(|&r| d.row(r).at_least(SLOW_CALL_BAND))
            .sum::<u64>(),
        "as many as its call rows counted from 1.05 ms up"
    );
    assert_eq!(
        (d.times.full_ns, d.times.barrier_ns, d.times.slow_count),
        (9_000_000, 0, 1)
    );
    assert_eq!(d.times.slow, later.times.slow, "the later ring, whole");
    assert_eq!(
        d.times.running, 2,
        "the later write's calls running, not a difference"
    );
    assert!(
        later
            .since(&later)
            .is_ok_and(|d| d.calls.count == 0 && d.calls.is_empty()),
        "no window, no calls"
    );
    let mut times_back = later;
    times_back.times.full_ns = 0;
    assert!(times_back
        .since(&earlier)
        .unwrap_err()
        .contains("full flush time"));
    let calls_back = Tally {
        calls: SlowCalls::default(),
        ..later
    };
    assert!(calls_back
        .since(&earlier)
        .unwrap_err()
        .contains("slow call count"));

    // A WINDOW OF MORE SLOW CALLS THAN A LINE KEEPS counts them all and keeps the last ones.
    let mut busy = earlier;
    for i in 0..CALLS_KEPT as u64 + 2 {
        busy.calls.push(call(
            Row::DirAppend,
            8_000_000_000 + i * 3_000_000,
            2_000_000,
        ));
    }
    let d = busy.since(&earlier).expect("one process");
    assert_eq!(
        (d.calls.count, d.calls.len()),
        (CALLS_KEPT as u64 + 2, CALLS_KEPT)
    );
    assert_eq!(
        d.calls.iter().next().map(|c| c.start_ns),
        Some(8_000_000_000 + 2 * 3_000_000),
        "the window's first two left the ring"
    );
}

/// TWO SOURCES MERGE BY END: the recorder's own calls and the state dir's, each in the order
/// it ended, merge into one ring in end order, the recorder's first on a tie, both counts
/// summed, and a merge past the ring keeps the last [`CALLS_KEPT`] of both.
#[test]
fn two_sources_of_slow_calls_merge_by_their_ends() {
    let of = |calls: &[SlowCall]| {
        let mut out = SlowCalls::default();
        for &c in calls {
            out.push(c);
        }
        out
    };
    let own = of(&[
        call(Row::Ack, 100, 2_000_000),
        call(Row::Commit, 100, 2_000_000),
        call(Row::Request, 5_000_000, 3_000_000),
    ]);
    let dir = of(&[
        call(Row::DirAppend, 0, 2_000_100),
        call(Row::LogpinFence, 1_000_000, 1_100_000),
    ]);
    let m = own.merged(&dir);
    assert_eq!(m.count, 5);
    assert_eq!(
        m.iter().map(|c| c.row).collect::<Vec<_>>(),
        [
            Row::Ack,
            Row::Commit,
            Row::DirAppend,
            Row::LogpinFence,
            Row::Request
        ],
        "{m:?}"
    );
    assert_eq!(dir.merged(&own).count, 5);
    let many = of(&(0..CALLS_KEPT as u64)
        .map(|i| call(Row::DirAppend, i * 10_000_000, 2_000_000))
        .collect::<Vec<_>>());
    let m = many.merged(&own);
    assert_eq!((m.count, m.len()), (CALLS_KEPT as u64 + 3, CALLS_KEPT));
    assert_eq!(
        m.iter().last(),
        many.iter().last(),
        "the last to end is the last kept"
    );
    assert!(
        m.iter().all(|c| c.row != Row::Ack),
        "the earliest ends left the ring"
    );
}

/// EVERY SLOW CALL IS KEPT WITH ITS START ON THE FLUSH CLOCK: a call of 1.05 ms or more is
/// kept, its start inside the clock readings taken around it less its length, whether or
/// not it is its row's slowest; a faster one, the park and a count row are not. An ack that
/// commits is one span in two rows, with one start. The write carries the recorder's calls
/// merged with the state dir's and the process's flush times, a flush ring that holds every
/// slow flush this test made (read after the write) among its last ones, or below the
/// oldest index it still holds. Off Apple the flush clock starts at the process's first
/// reading, so a start before it saturates at 0, as the recorder's does.
#[test]
fn every_slow_call_is_kept_with_its_start_on_the_flush_clock() {
    let dir = crate::test_scratch_dir("tally");
    let path = dir.path().join(FILE);
    let mut on = Recorder::at(&path).expect("open");
    let slow_ns = Hist::floor(SLOW_CALL_BAND, Unit::Ns);
    assert_eq!(slow_ns, 1 << 20, "1.05 ms");
    let before = astream_sys::clock_ns();
    on.span(Row::LaneSent, 3 * slow_ns);
    let after = astream_sys::clock_ns();
    let first = on.calls.iter().next().expect("kept");
    assert_eq!((first.row, first.ns), (Row::LaneSent, 3 * slow_ns));
    assert!(
        (before.saturating_sub(3 * slow_ns)..=after.saturating_sub(3 * slow_ns))
            .contains(&first.start_ns),
        "{first:?} does not start its length before a reading in {before}..={after}"
    );
    on.span(Row::LaneSent, slow_ns);
    on.span(Row::LaneSent, slow_ns - 1);
    let ago = |ms| Instant::now().checked_sub(Duration::from_millis(ms));
    on.note(Row::Park, ago(5));
    on.count(Row::DrainPosts, 3);
    assert_eq!(
        on.calls.iter().map(|c| (c.row, c.ns)).collect::<Vec<_>>(),
        [(Row::LaneSent, 3 * slow_ns), (Row::LaneSent, slow_ns)],
        "the second, under the row's largest, kept; the faster one, the park and the count not"
    );
    on.ack(ago(2), true);
    let (ack, commit) = (
        on.calls.kept[2].expect("the ack"),
        on.calls.kept[3].expect("the commit"),
    );
    assert_eq!(
        (ack.row, commit.row, ack.start_ns, ack.ns),
        (Row::Ack, Row::Commit, commit.start_ns, commit.ns),
        "one span"
    );
    let kept = Kept::default();
    kept.turn_on(true);
    let opened = astream_sys::clock_ns();
    kept.note(Row::AppendSelfAcked, 2 * slow_ns);
    let closed = astream_sys::clock_ns();
    kept.note(Row::DirAppend, 5_000);
    on.idle(3, |t| kept.copy_into(t));
    let written = Tally::parse(&std::fs::read_to_string(&path).expect("read")).expect("a line");
    assert_eq!(written.calls.count, 5);
    let dir_call = written
        .calls
        .iter()
        .find(|c| c.row == Row::AppendSelfAcked)
        .expect("the state dir's slow call");
    assert!(
        (opened.saturating_sub(2 * slow_ns)..=closed.saturating_sub(2 * slow_ns))
            .contains(&dir_call.start_ns),
        "the kept row's start: {written:?}"
    );
    assert_eq!(written.calls.iter().last(), Some(dir_call), "it ended last");
    let times = astream_sys::flush_times();
    assert!(written.times.full_ns <= times.full_ns && written.times.slow_count <= times.slow_count);
    // The ring keeps the last flushes by index, which each takes as it returns: one that
    // left it has an index below the oldest kept, whenever it began.
    assert!(
        written
            .times
            .slow()
            .all(|f| times.slow.contains(&f)
                || times.slow.first().is_some_and(|g| f.index < g.index)),
        "the written ring is the process's: {written:?} against {times:?}"
    );
}

/// THE PARTITION: the rows the loop is in one at a time, and only those,
/// sum to its accounted time; a commit (an ack), a call row (a replace), a
/// wake (inside a park) and a post's path are left out of it.
#[test]
fn only_the_disjoint_rows_partition_the_loops_time() {
    let t = sample();
    assert_eq!(
        t.partitioned_ns(),
        31_000_000 + 18_009_000 + 40_370_000 + 200_000 + 500_000_000
    );
    for row in [
        Row::Commit,
        Row::SetPostPin,
        Row::WakeEvent,
        Row::QueuedRecord,
        Row::PostPublish,
        Row::DrainPosts,
    ] {
        assert!(!row.partitions(), "{row:?}");
    }
    for row in [
        Row::LaneStatus,
        Row::LaneMeta,
        Row::LaneText,
        Row::LaneSessionsStatus,
    ] {
        assert!(row.partitions(), "a verb-lane round trip: {row:?}");
    }
    for (i, row) in Row::ALL.iter().enumerate() {
        assert_eq!(row.index(), i);
    }
}

/// Each write's head carries the process's astream-sys flush counts as of that write. A
/// flush alone is no move (any thread's flush moves the process's counts): NEGATIVE
/// CONTROL, the idle moment after a barrier with nothing else moved writes nothing, and the
/// next move's write carries the barrier.
#[cfg(target_vendor = "apple")]
#[test]
fn each_write_carries_the_processs_flushes_and_a_flush_alone_writes_nothing() {
    use std::io::Write as _;
    let dir = crate::test_scratch_dir("tally");
    let path = dir.path().join(FILE);
    let read = || std::fs::read_to_string(&path).expect("read the tally");
    let mut on = Recorder::at(&path).expect("open");
    let before = astream_sys::flush_tally();
    on.idle(5, |_| {});
    let first = Tally::parse(&read()).expect("a whole line");
    assert!(first.flushes.barrier >= before.barrier && first.flushes.full >= before.full);
    std::fs::write(&path, "sentinel").expect("overwrite");
    let mut file = std::fs::File::create(dir.path().join("f")).expect("a scratch file");
    file.write_all(b"ordered").expect("its bytes");
    let at_least = astream_sys::flush_tally().barrier + 1;
    astream_sys::barrier_sync(&file).expect("this volume takes the barrier");
    on.idle(5, |_| {});
    assert_eq!(read(), "sentinel", "a flush alone writes nothing");
    on.count(Row::DrainPosts, 1);
    on.idle(5, |_| {});
    let again = Tally::parse(&read()).expect("rewritten for the move");
    assert!(again.flushes.barrier >= at_least, "{again:?}");
    assert_eq!(
        again.since(&first).map(|d| d.row(Row::DrainPosts).count),
        Ok(1)
    );
}

/// Each verb the bridge's lane sends is counted by its head, a session's verb by the verb
/// after its address; the presence reads have rows of their own. CONTROL: a verb no arm
/// names, or a head that only begins like one, is still every other verb's.
#[test]
fn a_verb_lane_request_is_counted_by_its_head() {
    for (line, row) in [
        ("outbox earlier-receipts=1", Row::LaneOutbox),
        ("outbox", Row::LaneOutbox),
        ("outbox sent s-a 4 off=17", Row::LaneSent),
        ("outbox sent s-a 4 off=- reason=refused", Row::LaneSent),
        ("sessions bridge", Row::LaneSessions),
        ("deliver s-b receipt=3 off=9", Row::LaneDeliver),
        ("deliver s-b fetched=9 err=refused", Row::LaneDeliver),
        ("@s-a status", Row::LaneStatus),
        ("@s-a meta", Row::LaneMeta),
        ("@s-a text --json", Row::LaneText),
        ("@s-a text", Row::LaneText),
        ("sessions status", Row::LaneSessionsStatus),
        ("@s-a topic ls", Row::LaneOther),
        ("@s-a statusline", Row::LaneOther),
        ("@s-a", Row::LaneOther),
        ("@s-a  status", Row::LaneOther),
        ("status", Row::LaneOther),
        ("text --json", Row::LaneOther),
        ("sessions statuses", Row::LaneOther),
        ("sessions", Row::LaneOther),
        ("outboxes", Row::LaneOther),
        ("link mirror on", Row::LaneOther),
        ("delivered", Row::LaneOther),
    ] {
        assert_eq!(lane_row(line), row, "{line}");
    }
}

#[test]
fn an_off_recorder_counts_nothing_and_an_on_one_writes_only_what_moved() {
    let mut off = Recorder::default();
    off.request(Row::Request, Instant::now());
    let started = off.ack_starts();
    assert_eq!(started, None);
    assert_eq!(off.starts(), None);
    off.ack(started, true);
    off.note(Row::LaneDeliver, off.starts());
    off.count(Row::DrainPosts, 3);
    off.idle(9, |_| panic!("an off recorder copies nothing"));
    assert_eq!(off.tally(), Tally::default());

    let dir = crate::test_scratch_dir("tally");
    let path = dir.path().join(FILE);
    let mut on = Recorder::at(&path).expect("open");
    assert!(on.on());
    let read = || std::fs::read_to_string(&path).expect("read the tally");
    assert_eq!(read(), "", "nothing until the first idle moment");
    on.request(Row::Request, Instant::now());
    on.request(Row::Request, Instant::now());
    on.request(Row::BudgetRequest, Instant::now());
    let started = on.ack_starts();
    assert!(started.is_some());
    on.ack(started, false);
    let started = on.ack_starts();
    on.ack(started, true);
    let started = on.starts();
    on.note(Row::LaneSent, started);
    on.count(Row::DrainPosts, 2);
    let kept = Kept::default();
    drop(kept.timer(Row::DirUnlink));
    kept.turn_on(true);
    drop(kept.timer(Row::DirReplace));
    on.idle(17, |t| kept.copy_into(t));
    let written = Tally::parse(&read()).expect("a whole line");
    let counts = |t: &Tally, r: Row| t.row(r).count;
    assert_eq!(
        (
            written.pid,
            counts(&written, Row::Request),
            counts(&written, Row::BudgetRequest),
            counts(&written, Row::Commit),
            counts(&written, Row::Ack),
            counts(&written, Row::LaneSent),
            written.row(Row::DrainPosts).total,
        ),
        (std::process::id(), 2, 1, 1, 2, 1, 2),
        "a request is counted in the row it names, a budget read apart from the rest"
    );
    assert_eq!(
        (
            counts(&written, Row::DirReplace),
            counts(&written, Row::DirUnlink)
        ),
        (1, 0),
        "a kept row counts only once turned on"
    );
    assert_eq!(written.committed, 17);
    assert!(written.dir, "a test build counts the dir");
    assert!(written.row(Row::Commit).total <= written.row(Row::Ack).total);
    assert_eq!(
        counts(&written, Row::TallyWrite),
        0,
        "a write counts itself in the next line"
    );

    std::fs::write(&path, "sentinel").expect("overwrite");
    on.idle(17, |t| kept.copy_into(t));
    assert_eq!(read(), "sentinel");
    on.idle(18, |t| kept.copy_into(t));
    let again = Tally::parse(&read()).expect("rewritten whole");
    assert_eq!(again.committed, 18);
    assert_eq!(
        again.render(),
        read(),
        "the in-place line is the rendered one"
    );
    assert_eq!(again.row(Row::TallyWrite).count, 1);
    let d = again.since(&written).expect("one process");
    assert_eq!(d.row(Row::Request).count, 0);
    assert_eq!(d.row(Row::TallyWrite).count, 1, "the first write, counted");
    assert!(d.at_ns > 0, "the clock moved");
}

/// Occupied diagnostic names never authorize truncating their bytes or a
/// link target.
#[cfg(unix)]
#[test]
fn a_wait_tally_never_truncates_an_occupied_name_or_its_link_target() {
    let dir = crate::test_scratch_dir("tally");
    let fresh = dir.path().join("fresh-tally");
    let mut recorder = Recorder::at(&fresh).expect("fresh recorder");
    recorder.idle(7, |_| {});
    drop(recorder);
    assert_eq!(
        Tally::parse(&std::fs::read_to_string(&fresh).expect("fresh bytes"))
            .expect("fresh line")
            .committed,
        7
    );

    let occupied = dir.path().join("occupied");
    let unrelated = dir.path().join("unrelated");
    let linked = dir.path().join("linked-tally");
    std::fs::write(&occupied, b"previous diagnostic bytes").expect("owned occupied name");
    std::fs::write(&unrelated, b"unrelated owned sentinel").expect("owned sentinel");
    std::os::unix::fs::symlink(&unrelated, &linked).expect("owned link");
    let occupied_refused = Recorder::at(&occupied).is_err();
    let linked_refused = Recorder::at(&linked).is_err();
    assert!(
        occupied_refused
            && linked_refused
            && std::fs::read(&occupied).is_ok_and(|b| b == b"previous diagnostic bytes")
            && std::fs::read(&unrelated).is_ok_and(|b| b == b"unrelated owned sentinel")
            && std::fs::symlink_metadata(&linked).is_ok_and(|m| m.file_type().is_symlink()),
        "WAIT_TALLY_OCCUPIED_NAME_WAS_CLOBBERED"
    );
}
