// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn rows(states: &[u32]) -> Vec<u8> {
    states
        .iter()
        .flat_map(|f| f.to_le_bytes().into_iter().chain([0; 4]))
        .collect()
}

#[test]
fn dvfs_table_rejects_missing_partial_or_nonmonotonic_data() {
    assert_eq!(decode_table(&[]), None);
    assert_eq!(decode_table(&[0; 7]), None);
    assert_eq!(decode_table(&[0; 8]), None);
    let table = rows(&[1_020_000_000, 2_000_000_000]);
    assert_eq!(decode_table(&table), Some(vec![1020.0, 2000.0]));
    let mut wrong = table.clone();
    wrong.extend_from_slice(&table[..8]);
    let out_of_order = decode_table(&wrong).expect("decodes");
    assert_eq!(ordinal(out_of_order), None);
    assert_eq!(
        ordinal(decode_table(&table).expect("decodes")),
        Some(vec![1020.0, 2000.0])
    );
}

/// A table in Hz (M1–M3) and one in kHz (M4, M5) read the same MHz, and the range takes each
/// table's lowest and highest state whatever their order. CONTROL: the ordinal map refuses the
/// falling table the range reads.
#[test]
fn the_range_reads_hz_or_khz_tables_in_any_order() {
    assert_eq!(
        decode_table(&rows(&[972_000, 3_048_000])),
        decode_table(&rows(&[972_000_000, 3_048_000_000]))
    );
    let m5_super = decode_table(&rows(&M5_SUPER_KHZ)).expect("decodes");
    assert_eq!(span(&m5_super), (1308.0, 4464.0));
    let falling: Vec<f64> = m5_super.iter().rev().copied().collect();
    assert_eq!(span(&falling), (1308.0, 4464.0));
    assert_eq!(ordinal(falling), None, "control");
}

/// The M5's power-manager tables, kHz, as `ioreg -p IODeviceTree -n pmgr` gave them on a
/// MacBook Air (Mac17,4) on 2026-11-04: Super (`voltage-states5-sram`) is not rising at its top.
const M5_SUPER_KHZ: [u32; 19] = [
    1_308_000, 1_608_000, 1_932_000, 2_220_000, 2_508_000, 2_820_000, 3_072_000, 3_300_000,
    3_516_000, 3_720_000, 3_876_000, 4_020_000, 4_176_000, 4_248_000, 4_272_000, 4_332_000,
    4_428_000, 4_416_000, 4_464_000,
];
/// Efficiency (`voltage-states1-sram`), the same read.
const M5_EFFICIENCY_KHZ: [u32; 8] = [
    972_000, 1_152_000, 1_584_000, 1_992_000, 2_352_000, 2_700_000, 2_964_000, 3_048_000,
];

/// One channel's states as an M5 names them (`IDLE`, then `V{i}P{n-1-i}`), `idle` ticks idle
/// and each `(ordinal, ticks)` of `active` in that state.
fn channel(n: usize, idle: i64, active: &[(usize, i64)]) -> Vec<State> {
    let ticks = |i| {
        active
            .iter()
            .find(|(at, _)| *at == i)
            .map_or(0, |(_, t)| *t)
    };
    std::iter::once(State {
        name: "IDLE".into(),
        ticks: idle,
    })
    .chain((0..n).map(|i| State {
        name: format!("V{i}P{}", n - 1 - i),
        ticks: ticks(i),
    }))
    .collect()
}

/// Both of an M5's tables map as written, with no chip named: a channel's residency reads MHz,
/// each state at its own row's clock, the two the Super table writes out of rising order
/// included. The written and the rising order of that table differ, at every ordinal, by less
/// than the rule's bound, so the clock does not rest on which the states came in.
#[test]
fn an_m5_cpu_table_decodes_past_the_brand_gate() {
    let table = |khz: &[u32]| decode_table(&rows(khz)).and_then(ordinal);
    let p = table(&M5_SUPER_KHZ).expect("the Super table maps");
    let e = table(&M5_EFFICIENCY_KHZ).expect("the Efficiency table maps");
    assert_eq!(
        (p.len(), p[0], p[16], p[17], p[18]),
        (19, 1308.0, 4428.0, 4416.0, 4464.0)
    );
    assert_eq!((e.len(), e[0], e[7]), (8, 972.0, 3048.0));

    let mhz = |states: &[State], table: &[f64]| interpret(states, Some(table)).1;
    assert_eq!(mhz(&channel(19, 900, &[(16, 100)]), &p), Ok(4428.0));
    assert_eq!(mhz(&channel(19, 900, &[(17, 100)]), &p), Ok(4416.0));
    assert_eq!(mhz(&channel(19, 0, &[(0, 300), (18, 100)]), &p), Ok(2097.0));
    assert_eq!(mhz(&channel(8, 500, &[(7, 250), (5, 250)]), &e), Ok(2874.0));
    // CONTROL: the Super table does not fit the Efficiency channel's eight states.
    assert!(mhz(&channel(8, 500, &[(7, 250)]), &p).is_err());

    let mut rising = p.clone();
    rising.sort_by(f64::total_cmp);
    assert_ne!(rising, p, "control: the table is not in rising order");
    for (written, sorted) in p.iter().zip(&rising) {
        assert!(
            (written - sorted).abs() <= written * DIP / (1.0 - DIP),
            "{written} {sorted}"
        );
    }
}

/// A table with a fall deeper than the rule admits maps nothing: its channel keeps its activity
/// and reads no clock. The fall is judged against the highest state before it, not the state
/// beside it. CONTROL: each refused table's nearest admitted one maps.
#[test]
fn a_cpu_table_the_rule_cannot_order_maps_nothing() {
    let table = |khz: &[u32]| decode_table(&rows(khz)).and_then(ordinal);
    // The M5's Super table with its falling state lowered: 99 % of 4428 MHz is 4383.72.
    let lowered = |khz: u32| {
        let mut t = M5_SUPER_KHZ;
        t[17] = khz;
        t
    };
    assert_eq!(table(&lowered(4_383_000)), None);
    assert!(table(&lowered(4_384_000)).is_some(), "control");
    // Small falls that add up: 1975 is under 99 % of the 2000 three states before it.
    assert_eq!(
        table(&[1_000_000, 2_000_000, 1_990_000, 1_981_000, 1_975_000]),
        None
    );
    assert!(
        table(&[1_000_000, 2_000_000, 1_990_000, 1_981_000]).is_some(),
        "control"
    );
    // A table written fastest first, and one that climbs twice.
    let mut falling = M5_EFFICIENCY_KHZ;
    falling.reverse();
    assert_eq!(table(&falling), None);
    assert!(table(&M5_EFFICIENCY_KHZ).is_some(), "control");
    assert_eq!(table(&[1_020_000, 2_000_000, 1_020_000, 2_000_000]), None);
    // CONTROL: the second climb starting exactly 1 % down, the deepest fall that maps.
    assert!(table(&[1_020_000, 2_000_000, 1_980_000, 2_000_000]).is_some());

    let states = channel(19, 900, &[(16, 100)]);
    let refused = table(&lowered(4_383_000));
    let (active, mhz) = interpret(&states, refused.as_deref());
    assert_eq!(active, Ok(0.1));
    assert_eq!(mhz, Err("unread_format:frequency_table".into()));
    // CONTROL: the same channel reads its clock from the table as the M5 writes it.
    let (active, mhz) = interpret(&states, table(&M5_SUPER_KHZ).as_deref());
    assert_eq!((active, mhz), (Ok(0.1), Ok(4428.0)));
}

/// This Mac's power manager gives both tables, each a range a core clocks within.
#[test]
fn this_macs_dvfs_range_reads_or_says_why() {
    match dvfs_range() {
        Ok(r) => {
            for (lo, hi) in [r.e_mhz, r.p_mhz] {
                assert!((100.0..hi).contains(&lo) && hi < 10_000.0, "{r:?}");
            }
        }
        // An Intel Mac has no pmgr tables.
        Err(why) => assert!(why.starts_with("pmgr"), "{why}"),
    }
}
