// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// LAND'S STRICT READING, measured (`fixtures/land-defense-table.txt`):
// included into `turn_end_tests.rs`' `land_reading` module, so `super::*`
// is that module's (the policy's items and the tests' helpers).

/// The rows of `fixtures/land-defense-table.txt`: `(open, defense)`, a
/// literal `\n` in a row a line break.
fn land_table() -> Vec<(bool, String)> {
    let table = include_str!("fixtures/land-defense-table.txt");
    let mut rows = Vec::new();
    let mut open = None;
    for line in table.lines() {
        match line {
            "== open" => open = Some(true),
            "== clean" => open = Some(false),
            _ if line.starts_with('#') || line.trim().is_empty() => {}
            _ => rows.push((
                open.unwrap_or_else(|| panic!("a row before its section: {line:?}")),
                line.replace("\\n", "\n"),
            )),
        }
    }
    rows
}

/// Whether `reply` reads as its row says: an OPEN row is no clean defense
/// and names what it leaves (the nudge's suffix); a CLEAN row lands.
fn reads_as_labelled(open: bool, reply: &str) -> bool {
    if open {
        !clean_defense(Some(reply)) && !land_open_words(Some(reply)).is_empty()
    } else {
        clean_defense(Some(reply))
    }
}

/// LAND'S STRICT READING OVER THE TABLE: every OPEN defense — one that
/// admits any leftover, however phrased — reads open and names it, and every
/// CLEAN defense, the hard negations among them, lands. Both error rates are
/// named on a failure: a false landing (an OPEN row read clean) is the worst
/// failure Land has, a false non-landing (a CLEAN row read open) the second,
/// so neither direction may slip. NEGATIVE CONTROLS: the CLEAN rows.
#[test]
fn land_reads_the_defense_table_as_labelled() {
    let rows = land_table();
    let opens = rows.iter().filter(|(open, _)| *open).count();
    let cleans = rows.len() - opens;
    assert!(opens >= 80 && cleans >= 60, "{opens} open, {cleans} clean");
    let landed: Vec<&str> = rows
        .iter()
        .filter(|(open, reply)| *open && !reads_as_labelled(true, reply))
        .map(|(_, reply)| reply.as_str())
        .collect();
    let nudged: Vec<String> = rows
        .iter()
        .filter(|(open, reply)| !*open && !reads_as_labelled(false, reply))
        .map(|(_, reply)| {
            format!(
                "{reply:?} -> {:?}",
                open_work_strict(Some(&needs::defense_body(reply)))
            )
        })
        .collect();
    assert!(
        landed.is_empty() && nudged.is_empty(),
        "false landings {}/{opens}:\n{}\n\nfalse non-landings {}/{cleans}:\n{}",
        landed.len(),
        landed.join("\n"),
        nudged.len(),
        nudged.join("\n")
    );
}

/// EVERY FAMILY IS LOAD-BEARING (the mutation check): with any one family
/// of [`LAND_FAMILIES`] taken out, some OPEN defense of the table lands —
/// so no family is dead weight, and none can be dropped without the table
/// saying which defense it would let through.
#[test]
fn every_land_family_is_load_bearing() {
    let rows = land_table();
    for (skip, dropped) in LAND_FAMILIES.iter().enumerate() {
        let families: Vec<LandFamily> = LAND_FAMILIES
            .iter()
            .enumerate()
            .filter(|&(k, _)| k != skip)
            .map(|(_, f)| *f)
            .collect();
        let lands = |reply: &str| {
            needs::defense_line(reply).is_some()
                && needs::needs_you(reply).is_empty()
                && open_work_read(&needs::defense_body(reply), Some(&families)).is_empty()
        };
        assert!(
            rows.iter().any(|(open, reply)| *open && lands(reply)),
            "the {:?} family reads no OPEN row alone",
            dropped.name
        );
    }
}

/// THE NEEDS TABLE, READ AS LABELLED (`fixtures/needs-table.txt`): Land's
/// whole reading of each report, with its defense and a NEEDS YOU line or
/// block in some spelling. A NEED row names a need carrying its words, says
/// no none and lands nothing: a need lost is a false landing, the worst
/// failure Land has. A NONE row names no need, clears the standing set where
/// it holds a NEEDS YOU line ([`needs::says_no_needs`]) and lands: a phantom
/// need is a false non-landing. A HEADLESS row, an ask with no head, lands
/// nothing, and the nudge names what it read open. Every miss is named.
/// NEGATIVE CONTROLS: the open-work reading alone, needs left out, would
/// land some NEED row, and a reader that took every NEEDS YOU line for a
/// need would hold some NONE row.
#[test]
fn land_reads_the_needs_table_as_labelled() {
    use needs::NeedsRow;
    let rows = needs::needs_table();
    let count = |want: fn(&NeedsRow) -> bool| rows.iter().filter(|(row, _)| want(row)).count();
    let nones = count(|row| matches!(row, NeedsRow::None));
    let wanted = count(|row| matches!(row, NeedsRow::Need(_)));
    let asks = count(|row| matches!(row, NeedsRow::Headless(_)));
    assert!(
        nones >= 150 && wanted >= 150 && asks >= 20,
        "{nones} none, {wanted} need, {asks} headless"
    );
    let (mut lost, mut phantom, mut landed) = (Vec::new(), Vec::new(), Vec::new());
    for (row, reply) in &rows {
        let named = needs::needs_you(reply);
        let clean = clean_defense(Some(reply));
        let said = || format!("{reply:?} -> {named:?}");
        match row {
            NeedsRow::Need(words) => {
                let carried = named
                    .iter()
                    .any(|n| n.to_lowercase().contains(&words.to_lowercase()));
                if !carried || clean || needs::says_no_needs(reply) {
                    lost.push(said());
                }
            }
            NeedsRow::None => {
                let headed = needs::without_needs(reply) != *reply;
                if !named.is_empty() || !clean || headed && !needs::says_no_needs(reply) {
                    phantom.push(said());
                }
            }
            NeedsRow::Headless(_) => {
                if clean || land_open_words(Some(reply)).is_empty() {
                    landed.push(said());
                }
            }
        }
    }
    assert!(
        lost.is_empty() && phantom.is_empty() && landed.is_empty(),
        "lost needs {}/{wanted}:\n{}\n\nphantom needs {}/{nones}:\n{}\n\nheadless asks \
         landed or unnamed {}/{asks}:\n{}",
        lost.len(),
        lost.join("\n"),
        phantom.len(),
        phantom.join("\n"),
        landed.len(),
        landed.join("\n")
    );
    // NEGATIVE CONTROLS.
    assert!(rows.iter().any(|(row, reply)| {
        matches!(row, NeedsRow::Need(_))
            && open_work_strict(Some(&needs::defense_body(reply))).is_empty()
    }));
    assert!(rows.iter().any(|(row, reply)| {
        matches!(row, NeedsRow::None) && needs::without_needs(reply) != *reply
    }));
}

/// A NEED WRITTEN ANY WAY HOLDS THE LANDING: a NEEDS YOU line whose head
/// ends in a dash or carries a bracketed qualifier names a need, so the
/// defense beside it never lands however clean it reads; one that says none
/// stands, however it is marked, lands. NEGATIVE CONTROL: the same defense
/// with no NEEDS YOU line lands, and a need after a none word holds it.
#[test]
fn a_need_however_headed_holds_the_landing() {
    let defense = "Nothing left: all on origin/main.";
    assert!(clean_defense(Some(defense)));
    for reply in [
        "Nothing left: all on origin/main.\nNEEDS YOU — the Apple signing key to notarize the release.",
        "NEEDS YOU - publish the crate (needs the crates.io token).\nNothing left: everything else is on origin/main.",
        "Nothing left: all on origin/main.\nNEEDS YOU (owner): the Apple signing key.",
        "Nothing left: all on origin/main.\nNEEDS YOU: nothing, but the tag needs your key.",
    ] {
        assert!(!clean_defense(Some(reply)), "{reply:?}");
        assert_eq!(needs::needs_you(reply).len(), 1, "{reply:?}");
    }
    for none in [
        "NEEDS YOU: **none**",
        "NEEDS YOU: _nothing_",
        "NEEDS YOU: `none`",
        "NEEDS YOU:\n- none",
        "NEEDS YOU: nothing at the moment.",
        "NEEDS YOU: nothing at this time.",
        "NEEDS YOU: (none)",
        "NEEDS YOU: none, everything is landed.",
        "NEEDS YOU: nothing needed from you.",
        "NEEDS YOU: nothing from you.",
    ] {
        let reply = format!("{none}\n{defense}");
        assert!(clean_defense(Some(&reply)), "{reply:?}");
    }
}
