// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::corpora::land_table;

fn reads_as_labelled(open: bool, reply: &str) -> bool {
    if open {
        !clean_defense(Some(reply)) && !land_open_words(Some(reply)).is_empty()
    } else {
        clean_defense(Some(reply))
    }
}

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
    for report in [
        "The negative control failed to fail.",
        "The negative control fails to fail.",
        "The negative control failed to detect the bug.",
        "The negative control failed to run.",
        "The mutant fails to fail.",
        "The mutation run failed to start.",
    ] {
        let defense = format!("Nothing left: on main. {report}");
        assert!(!clean_defense(Some(&defense)), "{defense:?}");
        assert!(!land_open_words(Some(&defense)).is_empty(), "{defense:?}");
    }
    for report in [
        "The negative control gave 5 FAIL on exactly the new rows.",
        "Each of 8 mutants fails at least one test.",
        "The regression test fails on the old rule and passes on the new one.",
        "The regression test fails when the fix is reverted.",
        "The negative control failed to pass.",
        "The negative control failed to succeed.",
        "The previously failed negative control is fixed.",
        "The negative control failed to run and is now fixed.",
    ] {
        let defense = format!("Nothing left: on main. {report}");
        assert!(clean_defense(Some(&defense)), "{defense:?}");
    }
}

#[test]
fn every_land_family_is_load_bearing() {
    let open: Vec<(String, String)> = land_table()
        .into_iter()
        .filter(|(open, reply)| {
            *open && needs::defense_line(reply).is_some() && needs::needs_you(reply).is_empty()
        })
        .map(|(_, reply)| (reply.to_lowercase(), needs::defense_body(&reply)))
        .collect();
    for (skip, dropped) in LAND_FAMILIES.iter().enumerate() {
        let families: Vec<LandFamily> = LAND_FAMILIES
            .iter()
            .enumerate()
            .filter(|&(k, _)| k != skip)
            .map(|(_, f)| *f)
            .collect();
        let spells = |lower: &str| dropped.rows.iter().any(|row| lower.contains(row));
        let (named, rest): (Vec<_>, Vec<_>) = open.iter().partition(|(lower, _)| spells(lower));
        assert!(
            named
                .into_iter()
                .chain(rest)
                .any(|(_, body)| open_work_read(body, Some(&families)).is_empty()),
            "the {:?} family reads no OPEN row alone",
            dropped.name
        );
    }
}

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
    // Negative controls: the open-work reading alone, needs left out, would land some NEED
    // row, and a reader that took every NEEDS YOU line for a need would hold some NONE row.
    assert!(rows.iter().any(|(row, reply)| {
        matches!(row, NeedsRow::Need(_))
            && open_work_strict(Some(&needs::defense_body(reply))).is_empty()
    }));
    assert!(rows.iter().any(|(row, reply)| {
        matches!(row, NeedsRow::None) && needs::without_needs(reply) != *reply
    }));
}

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

#[test]
#[ignore = "a measurement: run with --ignored --nocapture; ATERM_LAND_HOLDOUT names another holdout"]
fn land_reads_a_holdout() {
    let text = std::env::var_os("ATERM_LAND_HOLDOUT").map_or_else(
        || super::corpora::HOLDOUT.to_string(),
        |path| std::fs::read_to_string(&path).expect("the holdout reads"),
    );
    let (mut opens, mut cleans, mut debatable) = (0, 0, 0);
    let (mut landed, mut nudged) = (Vec::new(), Vec::new());
    for (label, id, prov, reply) in super::corpora::holdout_cases(&text) {
        let clean = clean_defense(Some(&reply));
        let words = land_open_words(Some(&reply));
        let needs = needs::needs_you(&reply);
        match label {
            "open" => {
                opens += 1;
                if clean {
                    landed.push(format!("{id} ({prov}) {reply:?}"));
                }
            }
            "clean" => {
                cleans += 1;
                if !clean {
                    nudged.push(format!("{id} ({prov}) {words:?} {needs:?} {reply:?}"));
                }
            }
            "debatable" => {
                debatable += 1;
                println!("DEBATABLE {id} lands={clean} {words:?} {needs:?}");
            }
            _ => panic!("unknown label {label:?}"),
        }
    }
    for l in &landed {
        println!("FALSE LANDING {l}");
    }
    for n in &nudged {
        println!("FALSE NON-LANDING {n}");
    }
    println!(
        "holdout: {opens} open, {cleans} clean, {debatable} debatable: {} false landings, {} \
         false non-landings",
        landed.len(),
        nudged.len()
    );
}
