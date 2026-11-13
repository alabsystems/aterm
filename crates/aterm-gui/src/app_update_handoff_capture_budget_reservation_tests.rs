// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MAX_HANDOFF_CAPTURE_BUDGET_BYTES, optional_carry_fits};
use crate::seamless::{
    admit_checkpoint_dimensions, checkpoint_capture_budget_bytes, mandatory_checkpoint_cells,
    max_handoff_aggregate_grid_cells, max_handoff_history_lines,
};

fn walk_pool(rows: u16, cols: u16, sessions: u64) -> Result<u64, u64> {
    let carry = max_handoff_history_lines();
    let mandatory_cells = mandatory_checkpoint_cells(rows, cols);
    let mandatory_bytes = checkpoint_capture_budget_bytes(rows, cols, 0)
        .expect("PRECONDITION: the test geometry must be admissible at all");
    let history_cells = u64::from(cols) * u64::from(carry);
    let history_bytes = checkpoint_capture_budget_bytes(rows, cols, carry)
        .expect("PRECONDITION: the test geometry must be admissible with history")
        - mandatory_bytes;

    let mut cells_reserve = mandatory_cells * sessions;
    let mut bytes_reserve = mandatory_bytes * sessions;
    let mut used_cells = 0_u64;
    let mut used_bytes = 0_u64;
    let mut carried = 0_u64;
    for index in 0..sessions {
        cells_reserve -= mandatory_cells;
        bytes_reserve -= mandatory_bytes;
        let cells_fit = optional_carry_fits(
            used_cells,
            mandatory_cells,
            history_cells,
            cells_reserve,
            max_handoff_aggregate_grid_cells(),
        );
        let bytes_fit = optional_carry_fits(
            used_bytes,
            mandatory_bytes,
            history_bytes,
            bytes_reserve,
            MAX_HANDOFF_CAPTURE_BUDGET_BYTES,
        );
        let history = if cells_fit && bytes_fit { carry } else { 0 };
        let Ok(per_grid) = admit_checkpoint_dimensions(&mut used_cells, rows, cols, history, true)
        else {
            return Err(index);
        };
        used_bytes += per_grid * 2;
        if used_bytes > MAX_HANDOFF_CAPTURE_BUDGET_BYTES {
            return Err(index);
        }
        carried += u64::from(history != 0);
    }
    Ok(carried)
}

#[test]
fn a_heavy_pool_is_admitted_even_when_it_must_drop_history() {
    for (rows, cols) in [(49_u16, 110_u16), (60, 200)] {
        for sessions in [12_u64, 24, 64] {
            let carried = walk_pool(rows, cols, sessions).unwrap_or_else(|index| {
                panic!(
                    "session {index} of {sessions} at {rows}x{cols} was refused; a pool \
                     whose visible screens fit must never cost the update"
                )
            });
            assert!(
                carried > 0,
                "{sessions} panes at {rows}x{cols} carried no scrollback at all — the \
                 reservation must buy the pool a SMALLER carry, not abolish it"
            );
        }
    }
}

#[test]
fn a_pool_is_refused_only_when_its_mandatory_total_does_not_fit() {
    let (rows, cols) = (60_u16, 200_u16);
    let fits = max_handoff_aggregate_grid_cells() / mandatory_checkpoint_cells(rows, cols);
    assert!(
        walk_pool(rows, cols, fits).is_ok(),
        "{fits} panes at {rows}x{cols} are exactly what the aggregate holds \
         visible-only, so every one of them must be admitted"
    );
    assert_eq!(
        walk_pool(rows, cols, fits + 1),
        Err(fits),
        "one pane past the visible-only ceiling must be refused, and refused as the \
         LAST session — never an earlier one that lost its budget to somebody \
         else's scrollback"
    );
}

#[test]
fn every_full_screen_display_geometry_is_admitted_for_a_pool_of_panes() {
    use aterm_window::dpi::PhysicalSize;

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    {
        let ws = app.windows.get_mut(&wid).expect("the headless window");
        ws.metrics.pad = 24;
        ws.metrics.pad_top = 4;
        ws.metrics.head = 80;
    }
    let mut over_the_old_cap = Vec::new();
    for (width, height) in [
        (3024_u32, 1964_u32),
        (3456, 2234),
        (5120, 2880),
        (6016, 3384),
    ] {
        for (shape, size) in [
            ("full screen", PhysicalSize::new(width, height)),
            ("maximized", PhysicalSize::new(width, height - 74 - 140)),
        ] {
            let smallest = crate::FONT_PX_MIN as u32;
            for font_px in smallest..=48 {
                app.windows
                    .get_mut(&wid)
                    .expect("the headless window")
                    .metrics
                    .font_px = font_px as f32;
                let (rows, cols) = app.grid_dims_for(wid, size);
                let cells = u64::from(rows) * u64::from(cols);
                if cells > 32 * 1024 {
                    over_the_old_cap.push((width, height, shape, font_px, rows, cols));
                }
                for panes in 1..=8_u16 {
                    let pane_cols = (cols / panes).max(1);
                    let mut used = 0_u64;
                    for pane in 0..panes {
                        assert!(
                            admit_checkpoint_dimensions(&mut used, rows, pane_cols, 0, true)
                                .is_ok(),
                            "{width}x{height} {shape} at font {font_px} px ({rows}x{cols}): \
                             pane {pane} of {panes} ({rows}x{pane_cols}) was refused"
                        );
                    }
                }
            }
        }
    }
    assert!(
        !over_the_old_cap.is_empty(),
        "PRECONDITION: the table must reach past the old 32 Ki per-grid cap, or it \
         could not have caught the incident"
    );
}
