// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-zone sweep: `busy_zone_start` retains every row read by `busy_signal`.

use crate::fixture_files::fixture_screens;

fn blank_above(rows: &[String], start: usize) -> Vec<String> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| if i < start { String::new() } else { r.clone() })
        .collect()
}

fn exited_under(rows: &[String], row: usize) -> bool {
    let notice = aterm_phase::anchor!("exit.resume");
    rows[row..].iter().any(|r| r.trim_end() == notice)
}

fn check(name: &str, rows: &[String]) -> Result<(), String> {
    let start = aterm_phase::busy_zone_start(rows);
    if start > rows.len() {
        return Err(format!(
            "{name}: the zone starts at {start}, past {} rows",
            rows.len()
        ));
    }
    let boxed = aterm_phase::prompt_box_first_row(rows);
    match aterm_phase::phase::composer_rules(rows) {
        Some((top, _)) if !exited_under(rows, top) => {
            if start > top {
                return Err(format!(
                    "{name}: the zone starts at {start}, under the top rule {top}"
                ));
            }
            if let Some(b) = boxed.filter(|&b| start > b) {
                return Err(format!(
                    "{name}: the zone starts at {start}, under the box at {b}"
                ));
            }
            let whole = aterm_phase::busy_signal(rows);
            let zone = aterm_phase::busy_signal(&blank_above(rows, start));
            if whole != zone {
                return Err(format!(
                    "{name}: the busy reading moved when the rows above {start} were blanked: \
                     {whole:?} -> {zone:?}"
                ));
            }
        }
        Some(_) => {}
        None => {
            if let Some(b) = boxed.filter(|&b| start > b && !exited_under(rows, b)) {
                return Err(format!(
                    "{name}: the zone starts at {start}, under the box at {b}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn the_live_zone_keeps_every_row_the_busy_reading_reads() {
    let screens = fixture_screens("busy zone sweep");
    let cuts: Vec<(&str, &[String], usize)> = screens
        .iter()
        .flat_map(|(name, rows)| (0..rows.len()).map(move |k| (name.as_str(), &rows[..], k)))
        .collect();
    let bad: Vec<String> = crate::on_every_core(&cuts, |&(name, rows, k)| {
        check(&format!("{name} [{k}..]"), &rows[k..]).err()
    })
    .into_iter()
    .flatten()
    .collect();
    let framed = screens
        .iter()
        .filter(|(_, rows)| aterm_phase::phase::composer_rules(rows).is_some())
        .count();
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
    assert!(
        screens.len() > 100 && framed > 30,
        "{} screens, {framed} framed",
        screens.len()
    );
    assert!(screens.iter().any(|(n, _)| {
        std::path::Path::new(n).ends_with("claude-2.1.283-exit-relaunch-stale-busy.txt")
    }));
}
