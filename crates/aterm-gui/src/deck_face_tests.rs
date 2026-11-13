// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The deck header's size in points (`stats_plan`'s), which rule F's cases are written at.
const HEADER_PT: f64 = 9.5;

/// The ASCII table answers what a search of the sorted rows does, for every char of both
/// faces' tables and the chars around them, in both weights.
#[test]
fn the_ascii_table_answers_as_the_rows_do() {
    for m in [FaceMetrics::helvetica_neue(), FaceMetrics::test_stack()] {
        for ch in ('\0'..='\u{2100}').chain(['\u{1F600}']) {
            for bold in [false, true] {
                let searched = m
                    .advances
                    .binary_search_by_key(&ch, |&(c, _, _)| c)
                    .ok()
                    .and_then(|at| m.advances.get(at))
                    .map(|&(_, regular, heavy)| if bold { heavy } else { regular });
                assert_eq!(m.advance(ch, bold), searched, "{ch:?} bold={bold}");
            }
        }
        assert!(m.advance('~', true).is_some() && m.advance('…', false).is_some());
    }
}

#[test]
fn rule_f_takes_the_first_form_that_fits_else_cuts_the_last() {
    let m = FaceMetrics::helvetica_neue();
    let wide = m.width("Tab 2 · this tab", HEADER_PT, false);
    assert_eq!(
        m.fit(&["Tab 2 · this tab", "Tab 2"], wide, HEADER_PT, false),
        "Tab 2 · this tab"
    );
    assert_eq!(
        m.fit(&["Tab 2 · this tab", "Tab 2"], wide - 1.0, HEADER_PT, false),
        "Tab 2"
    );
    let cut = m.fit(
        &["a forty character tab label, cut to fit"],
        60.0,
        HEADER_PT,
        false,
    );
    assert!(cut.ends_with('…') && m.width(&cut, HEADER_PT, false) <= 60.0);
    assert!(!cut.contains(" …"));
    assert_eq!(m.fit(&["Performance"], 1.0, HEADER_PT, false), "");
}

#[test]
fn a_measured_text_fits_as_the_face_measures_it() {
    let m = FaceMetrics::helvetica_neue();
    let forms = txts(["Tab 2 · this tab".to_owned(), "Tab 2".to_owned()], &m);
    for bold in [false, true] {
        for t in &forms {
            let face = m.width(&t.s, HEADER_PT, bold);
            assert!(
                (t.w(HEADER_PT, bold) - face).abs() < 1e-9,
                "{:?} bold {bold}: measured once {}, the face {face}",
                t.s,
                t.w(HEADER_PT, bold)
            );
        }
        let wide = m.width("Tab 2 · this tab", HEADER_PT, bold);
        assert_eq!(fit(&forms, wide, HEADER_PT, bold, &m), "Tab 2 · this tab");
        assert_eq!(fit(&forms, wide - 1.0, HEADER_PT, bold, &m), "Tab 2");
        let short = m.width("Tab 2", HEADER_PT, bold) - 1.0;
        let cut = fit(&forms, short, HEADER_PT, bold, &m);
        assert_eq!(cut, m.fit(&["Tab 2"], short, HEADER_PT, bold));
        assert!(cut.ends_with('…') && cut.len() > '…'.len_utf8(), "{cut:?}");
    }
    assert_ne!(
        m.width("Tab 2 · this tab", HEADER_PT, false),
        m.width("Tab 2 · this tab", HEADER_PT, true),
        "the case tells regular from bold"
    );
    assert_eq!(fit(&[], 100.0, HEADER_PT, false, &m), "");
}

/// Prints [`HELVETICA_NEUE`] from the face the macOS chrome draws in and
/// checks the embedded table against it: run it by name with
/// `--nocapture` after a macOS font update, and paste the
/// array it prints over the table.
#[cfg(target_os = "macos")]
#[test]
fn print_the_helvetica_neue_table() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let regular = crate::tray_raster::ui_font_of(TextFace::Ui).expect("the chrome's UI face");
    let bold = crate::tray_raster::ui_font_of(TextFace::UiBold).expect("the chrome's bold");
    let upem = f64::from((1.0 / regular.scale_factor(1.0)).round());
    assert_eq!(upem, HELVETICA_NEUE_UPEM);
    let units = |font: &Font, ch: char| {
        assert!(font.maps(ch), "Helvetica Neue maps {ch:?}");
        (em_advance(font, ch) * upem).round() as u16
    };
    let rows: Vec<(char, u16, u16)> = deck_glyphs()
        .map(|ch| (ch, units(&regular, ch), units(&bold, ch)))
        .collect();
    println!(
        "const HELVETICA_NEUE: [(char, u16, u16); {}] = [",
        rows.len()
    );
    for (ch, regular, bold) in &rows {
        println!("    ({ch:?}, {regular}, {bold}),");
    }
    println!("];");
    assert_eq!(
        rows, HELVETICA_NEUE,
        "paste the printed table over HELVETICA_NEUE"
    );
}

#[test]
fn the_deck_measures_its_words_as_the_pen_draws_them() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let m = deck_metrics();
    for s in [
        "Tokens",
        "learning peak",
        "low, 41 GB free",
        "aterm freed 41 GB today",
        "AVATAR To We",
    ] {
        for (face, bold) in [
            (crate::widget::TextFace::Ui, false),
            (crate::widget::TextFace::UiBold, true),
        ] {
            let pen = f64::from(crate::tray_raster::ui_text_width_for(face, s, 19.0));
            let planned = m.width(s, 19.0, bold);
            assert!(
                (pen - planned).abs() < 0.01,
                "{s:?} bold {bold}: the pen draws {pen}, the deck plans {planned}"
            );
        }
    }
}
