// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;
use aterm_types::{WindowOperation, WindowResponse};

fn geometry_read(term: &mut Terminal) -> String {
    term.process(b"\x1b[?2;1S");
    String::from_utf8(term.take_response().expect("XTSMGRAPHICS always answers")).expect("ASCII")
}

#[test]
fn sixel_geometry_read_reports_the_text_area_with_window_ops() {
    let mut term = Terminal::new(24, 80);
    term.modes_mut().allow_window_ops = true;
    term.set_cell_pixel_size(9, 19);
    assert_eq!(geometry_read(&mut term), "\x1b[?2;0;720;456S");
}

#[test]
fn sixel_geometry_read_does_not_reach_the_host_without_window_ops() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(9, 19);
    term.set_window_callback(|op| match op {
        WindowOperation::ReportTextAreaSizePixels => Some(WindowResponse::SizePixels {
            height: 111,
            width: 222,
        }),
        _ => None,
    });
    assert_eq!(geometry_read(&mut term), "\x1b[?2;0;1000;1000S");

    term.modes_mut().allow_window_ops = true;
    assert_eq!(geometry_read(&mut term), "\x1b[?2;0;222;111S");
}

#[test]
fn the_advertised_sixel_maximum_is_placed() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2;4S");
    let reply = String::from_utf8(term.take_response().expect("an answer")).unwrap();
    let dims: Vec<usize> = reply
        .trim_start_matches("\x1b[?2;0;")
        .trim_end_matches('S')
        .split(';')
        .map(|n| n.parse().expect("a dimension"))
        .collect();
    let (w, h) = (dims[0], dims[1]);
    let bands = h / 6;
    let rle = format!(
        "\"1;1;{w};{h}#1;2;100;0;0#1{}",
        format!("!{w}~-").repeat(bands)
    );
    let columns = format!(
        "#1;2;100;0;0#1{}",
        format!("{}-", "~".repeat(w)).repeat(bands)
    );
    let pass = |c: usize| format!("#{c}{}$", "~".repeat(w));
    let passes = (0..bands)
        .map(|_| (1..=16).map(pass).collect::<String>() + "-")
        .collect::<String>();
    for (name, body) in [("rle", rle), ("columns", columns), ("passes", passes)] {
        let mut term = Terminal::new(24, 80);
        term.process(format!("\x1bP0;1;0q{body}\x1b\\").as_bytes());
        assert!(
            !term.images_row(0).is_empty(),
            "{name}: the advertised {w}x{h} placed nothing"
        );
    }
}

#[test]
fn sixel_geometry_read_falls_back_without_a_cell_box() {
    let mut term = Terminal::new(24, 80);
    term.modes_mut().allow_window_ops = true;
    assert_eq!(geometry_read(&mut term), "\x1b[?2;0;1000;1000S");
}
