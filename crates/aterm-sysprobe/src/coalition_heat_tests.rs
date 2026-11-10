// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The heat self-check on records this kernel wrote (xnu-13432, macOS 27.0.1, Apple M5): a busy
//! coalition read twice 1.0045 s apart, and one whose lifetime Super time passed its `cpu_time`.
//! The record half is this crate's ([`decode`], [`CoalHeat`]); the judgement is the model's
//! (`aterm_vitals::heat::judge`), so these hold the two together.

use super::*;
use aterm_vitals::heat::{Busy, ClockRange, Counters, Judged, judge};

/// Coalition 1513 at the first read (`coal_clocks record`, 2026-11-06).
#[rustfmt::skip]
const THEN: [u64; USAGE_WORDS] = [
    9073852, 9073797, 295617966608791, 20207814381668, 105942679, 200883, 2180775223296,
    3820829401088, 1621519667073, 18907824889, 1073594260, 973771528992410, 36491214848,
    4422455454226, 475136, 282041147392, 0, 0, 0, 0, 266233868279, 28615895465, 11020557129891, 7,
    236392550926, 0, 1587956940, 675728369717, 20151601957514, 10573911590, 27515543287,
    5790897696406639, 2289075504235860, 282041147392, 0, 3657548405202129, 1172551389787047, 0,
    1617605702, 117825392144, 2401806400, 4137515388415, 0, 0, 6963511296, 5289427168, 9172699,
    99805984, 411819698, 18323, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN,
    UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN,
];

/// Coalition 1513 at the second read.
#[rustfmt::skip]
const NOW: [u64; USAGE_WORDS] = [
    9073864, 9073809, 295618970672833, 20207887882069, 105943018, 200883, 2180775223296,
    3820829409280, 1621519667073, 18907824889, 1073594260, 973774999564395, 36491214848,
    4422455486994, 475136, 282041200640, 0, 0, 0, 0, 266233868279, 28615895465, 11020611068373, 7,
    236392702586, 0, 1587956940, 675728369717, 20151675540352, 10573911590, 27515543287,
    5790922886825048, 2289082633754765, 282041200640, 0, 3657568549973155, 1172556299047712, 0,
    1617605702, 117825392144, 2402314304, 4137515388415, 0, 0, 6963511296, 5289429562, 9172761,
    99805984, 411819714, 18323, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN,
    UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN,
];

/// Coalition 7154, whose lifetime `cpu_ptime` (word 22) is above its `cpu_time` (word 3).
#[rustfmt::skip]
const P_PAST_CPU_TIME: [u64; USAGE_WORDS] = [
    58221, 58221, 174867923821000, 1674463238876, 6766404, 4018, 21094785024, 16984256512,
    1741343369535, 3097663933, 654710062, 121339951043798, 0, 18374021313, 0, 3887747072, 0, 0, 0,
    0, 35999402840, 1811864733, 1741710369953, 7, 891973193, 0, 228383188, 67933182799,
    2426329664217, 34906403445, 66354974169, 675241355711229, 280716595955798, 3887747072, 0,
    499931233722518, 187646151634890, 0, 0, 0, 0, 1776163608821, 0, 0, 33243136, 1071575534,
    2392626, 739784, 2694393, 0, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN,
    UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN, UNWRITTEN,
];

const WINDOW_NS: u64 = 1_004_535_917;

/// Apple Silicon's Mach timebase (24 MHz).
const TIMEBASE: (u32, u32) = (125, 3);

/// This M5's pmgr tables: Super 1308–4464 MHz, Efficiency 972–3048 MHz.
const M5: ClockRange = ClockRange {
    p_mhz: (1308.0, 4464.0),
    e_mhz: (972.0, 3048.0),
};

/// The busy core time that matches the pair: its own `cpu_time` and Super time.
const BUSY: Busy = Busy {
    all_ns: 3_062_516_708,
    p_ns: Some(2_247_436_750),
};

fn read(records: &[(u64, [u64; USAGE_WORDS])], newest: u64) -> HeatRead {
    HeatRead::of(&RecordRead {
        listed: u32::try_from(records.len()).expect("few"),
        newest: Some(newest),
        truncated: false,
        records: records.to_vec(),
    })
}

fn counters(heat: Heat) -> Counters {
    Counters::from_words(heat.words())
}

fn judged(then: &[u64; USAGE_WORDS], now: &[u64; USAGE_WORDS]) -> Result<Judged, &'static str> {
    let m = moved(&read(&[(1513, *then)], 1513), &read(&[(1513, *now)], 1513))?;
    judge(&counters(m.sum), WINDOW_NS, TIMEBASE, Some(&M5), Some(BUSY))
}

fn near(v: Option<f64>, want: f64) -> bool {
    v.is_some_and(|v| (v - want).abs() < 0.05)
}

#[test]
fn a_recorded_pair_decodes_its_words_and_judges_within_the_chip() {
    let j = judged(&THEN, &NOW).expect("the recorded layout");
    assert!(near(j.mhz, 2327.99 * j.cpu_s / j.qos_s), "{j:?}");
    assert!(near(j.p_mhz, 2184.38), "{j:?}");
    assert!(near(j.e_mhz, 2691.76), "{j:?}");
    assert!(!j.e_refused);
    assert!(near(j.ipc, 3.53), "{j:?}");
    assert!((j.cpu_w - 3.4549).abs() < 1e-3, "{j:?}");
    assert_eq!((j.gpu_w, j.ane_w, j.ane_busy), (0.0, 0.0, 0.0));
    assert!((j.cpu_s - 3.0625).abs() < 1e-3 && (j.p_s - 2.2474).abs() < 1e-3);
}

/// A layout whose words moved by one or two (a field added or dropped before them, the QoS count
/// still in its place) reads as unread, never as a shifted value: every word read, or only those
/// after the QoS times (a field added after them). CONTROL: unshifted, the same pair judges.
#[test]
fn a_shifted_layout_reads_unread_never_a_shifted_value() {
    let shift = |raw: &[u64; USAGE_WORDS], by: isize, from: usize| {
        let mut out = *raw;
        for &w in WORDS.iter().filter(|&&w| w >= from) {
            out[w] = raw[w.checked_add_signed(by).expect("in the record")];
        }
        out
    };
    assert!(judged(&shift(&THEN, 0, 0), &shift(&NOW, 0, 0)).is_ok());
    for from in [0, EQOS + QOS_CLASSES] {
        for by in [-2, -1, 1, 2] {
            let got = judged(&shift(&THEN, by, from), &shift(&NOW, by, from));
            assert!(got.is_err(), "words from {from} moved by {by}: {got:?}");
        }
    }
}

/// The busy cross-check refuses a time word moved onto a counter that barely moves: the shifted
/// `cpu_time` reads 14 µs of a window the cores were busy 3.06 s. Without the busy time the
/// whole clock over the QoS time refuses it as well (0.02 MHz, `clock_range`), where N1's judge,
/// whose clocks read `cpu_time`, judged it an idle coalition. CONTROL: unshifted, the same busy
/// time judges.
#[test]
fn a_time_word_moved_onto_a_still_counter_is_refused_by_the_busy_time() {
    let shift = |raw: &[u64; USAGE_WORDS]| {
        let mut out = *raw;
        for &w in &WORDS {
            out[w] = raw[w + 1];
        }
        out
    };
    let (then, now) = (shift(&THEN), shift(&NOW));
    let m = moved(&read(&[(1513, then)], 1513), &read(&[(1513, now)], 1513)).expect("moves");
    assert_eq!(
        judge(&counters(m.sum), WINDOW_NS, TIMEBASE, Some(&M5), Some(BUSY)),
        Err("busy_time")
    );
    assert_eq!(
        judge(&counters(m.sum), WINDOW_NS, TIMEBASE, Some(&M5), None),
        Err("clock_range")
    );
    assert!(judged(&THEN, &NOW).is_ok(), "control");
}

/// The record's times are separate accounts: a lifetime Super time above `cpu_time` still decodes,
/// while Super cycles above all cycles do not.
#[test]
fn super_time_past_cpu_time_decodes_and_super_cycles_past_cycles_do_not() {
    let heat = decode(&P_PAST_CPU_TIME).expect("decodes");
    assert!(heat.p_ticks > heat.cpu_ticks && heat.qos_ticks > heat.p_ticks);
    let mut over = P_PAST_CPU_TIME;
    over[P_CYCLES] = over[CYCLES] + 1;
    assert_eq!(decode(&over), Err("super_over_whole"));
    let mut cut = THEN;
    cut[GPU_ENERGY] = UNWRITTEN;
    assert_eq!(decode(&cut), Err("unwritten"));
    let mut other = THEN;
    other[EQOS_LEN] = 6;
    assert_eq!(decode(&other), Err("layout"));
}

/// A coalition whose counter ran backwards refuses the reading; one born since counts whole; one
/// at or below the earlier newest id that it did not read is left out; one gone is dropped.
#[test]
fn the_change_matches_by_id_and_refuses_a_counter_that_ran_backwards() {
    let then = read(&[(10, THEN), (20, THEN)], 30);
    let now = read(&[(10, NOW), (25, THEN), (40, THEN)], 40);
    let m = moved(&then, &now).expect("moves");
    assert_eq!((m.matched, m.born, m.unbased), (1, 1, 1));
    let whole = decode(&THEN).expect("decodes");
    let change = decode(&NOW)
        .expect("decodes")
        .since(whole)
        .expect("forward");
    assert_eq!(m.sum, change.plus(whole));
    assert_eq!(
        moved(&read(&[(10, NOW)], 10), &read(&[(10, THEN)], 10)),
        Err("backwards")
    );
    let mut refused = read(&[(10, NOW)], 10);
    refused.refused = Some(("layout", 1));
    assert_eq!(moved(&then, &refused), Err("refused_record"));
}

/// A refill keeps its rows ascending by id, one per id, and counts the records it refused.
#[test]
fn a_refill_sorts_its_rows_and_counts_its_refusals() {
    let mut other = THEN;
    other[EQOS_LEN] = 6;
    let mut r = HeatRead::default();
    r.refill(&RecordRead {
        listed: 4,
        newest: Some(30),
        truncated: false,
        records: vec![(30, NOW), (10, THEN), (30, THEN), (20, other)],
    });
    assert_eq!(r.rows.iter().map(|x| x.0).collect::<Vec<_>>(), [10, 30]);
    assert_eq!(r.refused, Some(("layout", 1)));
}

/// The word order the copy loop sums in is the order the model names: every counter of a recorded
/// record lands on the model's field of the same name. CONTROL: two words swapped land on each
/// other's fields.
#[test]
fn the_words_land_on_the_models_names() {
    let heat = decode(&THEN).expect("decodes");
    let named = |c: Counters| {
        [
            c.cpu_ticks,
            c.qos_ticks,
            c.p_ticks,
            c.energy_nj,
            c.instructions,
            c.cycles,
            c.p_instructions,
            c.p_cycles,
            c.ane_ticks,
            c.ane_energy_nj,
            c.gpu_energy_nj,
        ]
    };
    let ours = [
        heat.cpu_ticks,
        heat.qos_ticks,
        heat.p_ticks,
        heat.energy_nj,
        heat.instructions,
        heat.cycles,
        heat.p_instructions,
        heat.p_cycles,
        heat.ane_ticks,
        heat.ane_energy_nj,
        heat.gpu_energy_nj,
    ];
    assert_eq!(named(counters(heat)), ours);
    assert_eq!(COUNTERS, aterm_vitals::heat::COUNTERS);
    let mut swapped = heat.words();
    swapped.swap(3, 4);
    assert_ne!(named(Counters::from_words(swapped)), ours, "control");
}

/// The copy loop keeps each record's words by id in the kernel's order and sums them; one record
/// that does not decode refuses the read's heat and keeps nothing. CONTROL: without it, the same
/// records sum.
#[test]
fn the_copy_loop_sums_what_decoded_and_refuses_a_read_with_one_that_did_not() {
    let usage = |id| match id {
        30 => Some(NOW),
        10 => Some(THEN),
        20 => Some(P_PAST_CPU_TIME),
        _ => None,
    };
    let read =
        crate::coalitions::read_of(&[(30, 1), (10, 1), (20, 1)], false, usage, TIMEBASE).heat;
    let words = |raw: &[u64; USAGE_WORDS]| decode(raw).expect("decodes").words();
    assert_eq!(
        read.by_id,
        [
            (30, words(&NOW)),
            (10, words(&THEN)),
            (20, words(&P_PAST_CPU_TIME))
        ]
    );
    let mut total = [0u64; COUNTERS];
    for (_, w) in &read.by_id {
        for (t, v) in total.iter_mut().zip(w) {
            *t += v;
        }
    }
    assert_eq!(read.total, Ok(total));
    let mut other = THEN;
    other[EQOS_LEN] = 6;
    let refused = crate::coalitions::read_of(
        &[(30, 1), (10, 1)],
        false,
        |id| (id == 30).then_some(NOW).or(Some(other)),
        TIMEBASE,
    )
    .heat;
    assert_eq!((refused.total, refused.by_id.len()), (Err("layout"), 0));
}

/// A total that would pass `u64` refuses the read's heat (`overflow`) and keeps nothing, never a
/// wrapped sum that the change would read as a shifted value. CONTROL: the same records less one
/// instruction each sum.
#[test]
fn a_total_past_u64_refuses_the_read_never_a_wrapped_sum() {
    let big = |instructions| {
        let mut raw = NOW;
        raw[INSTRUCTIONS] = instructions;
        raw
    };
    let half = u64::MAX / 2 + 1;
    let past =
        crate::coalitions::read_of(&[(10, 1), (20, 1)], false, |_| Some(big(half)), TIMEBASE).heat;
    assert_eq!((past.total, past.by_id.len()), (Err("overflow"), 0));
    let within = crate::coalitions::read_of(
        &[(10, 1), (20, 1)],
        false,
        |_| Some(big(half - 1)),
        TIMEBASE,
    )
    .heat;
    let instructions = decode(&big(half - 1)).expect("decodes").words();
    let at = instructions
        .iter()
        .position(|&w| w == half - 1)
        .expect("the instructions word");
    assert_eq!(within.total.map(|t| t[at]), Ok(u64::MAX - 1), "control");
}

/// The folded form (the copy loop's totals, the start counter's merge handing back the gone
/// coalitions, their last words off the earlier total) equals the change by coalition id that a
/// pass after the read gives ([`moved`]), over a matched, a gone and a born coalition. CONTROL:
/// leaving the gone coalition on the earlier total runs the counters backwards.
#[test]
fn the_folded_change_equals_the_change_by_id() {
    use aterm_vitals::heat::HeatBook;
    use aterm_vitals::starts::{CoalTaskRead, CoalTasks, StartCount};
    let then_records = [(10, THEN), (20, NOW)];
    let now_records = [(30, P_PAST_CPU_TIME), (10, NOW)];
    let fold = |records: &[(u64, [u64; USAGE_WORDS])], at_ns: u64| {
        let listed: Vec<(u64, u32)> = records.iter().map(|&(id, _)| (id, 1)).collect();
        let read = crate::coalitions::read_of(
            &listed,
            false,
            |id| records.iter().find(|r| r.0 == id).map(|r| r.1),
            TIMEBASE,
        );
        let tasks = CoalTaskRead::of(
            read.newest,
            read.truncated,
            read.rows.iter().map(|u| CoalTasks {
                id: u.id,
                started: u.tasks_started,
                exited: u.tasks_exited,
            }),
        );
        StartCount::Coalitions {
            read: tasks.with_heat(HeatBook {
                at_ns,
                timebase: read.heat.timebase,
                by_id: read.heat.by_id,
                total: read.heat.total,
            }),
            own: None,
        }
    };
    let (then, now) = (fold(&then_records, 0), fold(&now_records, WINDOW_NS));
    let folded = aterm_vitals::starts::moved(&then, &now)
        .expect("counts")
        .heat
        .expect("carries heat")
        .expect("changes");
    let by_id = moved(&read(&then_records, 20), &read(&now_records, 30)).expect("moves");
    assert_eq!((by_id.matched, by_id.born), (1, 1));
    assert_eq!(folded.counters, counters(by_id.sum));
    assert_eq!(folded.window_ns, WINDOW_NS);
    let (StartCount::Coalitions { read: a, .. }, StartCount::Coalitions { read: b, .. }) =
        (&then, &now)
    else {
        unreachable!("coalition reads")
    };
    let (a, b) = (a.heat().expect("heat"), b.heat().expect("heat"));
    assert_eq!(
        aterm_vitals::heat::change(a, b, &[]),
        Err("backwards"),
        "control"
    );
}
