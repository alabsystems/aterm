// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

fn linked(rows: u16) -> aterm_core::terminal::Terminal {
    let mut t = aterm_core::terminal::Terminal::new(rows, 200);
    let url_tail = "a".repeat(2090);
    for row in 0..rows {
        let mut line = Vec::new();
        for link in 0..45 {
            line.extend_from_slice(
                format!("\x1b]8;;https://e/{row}-{link}-{url_tail}\x07X\x1b]8;;\x07").as_bytes(),
            );
        }
        if row + 1 < rows {
            line.extend_from_slice(b"\r\n");
        }
        t.process(&line);
    }
    t
}

#[test]
fn a_held_screen_the_writer_refuses_spends_none_of_the_held_budget() {
    let grid = |rows| {
        u64::try_from(
            linked(rows)
                .checkpoint_carry_abandoning_partial(0)
                .0
                .grid
                .len(),
        )
        .expect("a length")
    };
    assert!(
        3 * grid(50) <= crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES
            && 3 * grid(50) + grid(30) > crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES,
        "{} {}",
        grid(50),
        grid(30)
    );
    let mut app = crate::App::headless_for_test();
    let mut ids = Vec::new();
    for rows in [50, 50, 50, 30] {
        let id = app.next_session_id;
        let session = crate::ptyless_session(
            id,
            aterm_session::SessionId::generate(),
            linked(rows),
            Arc::new(crate::SinkWriter::new(-1)),
        );
        app.push_stub_tab(crate::WindowId(0), session);
        ids.push(id);
    }
    let (dense, fits) = (&ids[..3], ids[3]);
    let held = app.capture_held_screens(
        &[],
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        &mut 0,
        crate::seamless::WireCaps::current(),
        aterm_update_core::handoff_policy::CarryCeiling::Full,
        &mut Vec::new(),
    );
    let captured: Vec<u64> = held.iter().map(|screen| screen.local_id).collect();
    assert!(
        captured.contains(&fits),
        "the pane that fits is carried: {captured:?}"
    );
    assert!(
        !captured.iter().any(|id| dense.contains(id)),
        "screens the writer refuses are not captured: {captured:?}"
    );
}
