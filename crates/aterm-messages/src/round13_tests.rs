// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the round-13 engine invariants (design rulings 249–252) — a
//! row's decision buttons pinned to the right edge with `Details ›` last at
//! every width (ruling 252), and a moving comet that leaves the glyph cell
//! to the row's own glyph (ruling 251).

use crate::animate::{Anim, Look};
use crate::center::MessageCenter;
use crate::glass::{Links, Presentation};
use crate::log::MessageLog;
use crate::model::{Decision, Hold, Intent, Message, Meter, Severity, WallStamp, tags};
use crate::text::char_width;
use crate::{Duration, HOLD_ASK, Instant, MARGIN, STALE_TAILED, STALE_UPDATE};

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

fn present(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &char_width, None, Links::Painted)
}

/// The rows that carry decision buttons: an ask with a Primary and a
/// Secondary, a metered download with a Primary, and a busy row with one.
fn rows_with_buttons() -> Vec<Message> {
    vec![
        Message::new(
            tags::UPDATE,
            Severity::Warn,
            "aterm v0.93.0 is ready to install",
        )
        .action(Intent::ApplyUpdate { build: 1234 })
        .action(Intent::NotNow {
            decision: Decision::FileAccess,
        })
        .hold(Hold::Ask { for_: HOLD_ASK }),
        Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.93.0")
            .meter(Meter {
                fill_permille: Some(420),
                ..Meter::default()
            })
            .action(Intent::ApplyUpdate { build: 1234 })
            .hold(Hold::Live {
                stale_after: STALE_UPDATE,
            }),
        Message::new(tags::PACKAGES, Severity::Info, "Installing Homebrew")
            .meter(Meter::busy(""))
            .action(Intent::NewWindow)
            .hold(Hold::Live {
                stale_after: STALE_TAILED,
            }),
    ]
}

/// PINNED RIGHT (ruling 252, the owner: "Pinned right"): on every width a
/// row's buttons end exactly at the right margin, in their authored order,
/// with `Details ›` LAST whenever it is painted — a wide window never lets
/// them drift toward the words, and a narrow one sheds from the words first.
#[test]
fn a_rows_buttons_are_pinned_right_with_details_last_at_every_width() {
    for msg in rows_with_buttons() {
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        c.post(msg.clone(), stamp(), now);
        c.commit_rows(now, 3);
        for cols in 30..=320usize {
            let p = present(&c, cols);
            let row = &p.rows[0];
            let caps = &row.capsules;
            assert!(!caps.is_empty(), "{}@{cols}: buttons", msg.title);
            // Below the degenerate step the capsules may start at column 0
            // (the documented best effort under ~24 cols); every width here
            // is above it.
            let last = caps.last().unwrap();
            assert_eq!(
                last.col + last.width,
                cols - MARGIN,
                "{}@{cols}: the last button ends at the right margin",
                msg.title
            );
            for pair in caps.windows(2) {
                assert!(
                    pair[0].col + pair[0].width < pair[1].col,
                    "{}@{cols}: in order, apart",
                    msg.title
                );
            }
            if let Some(k) = caps.iter().position(|cap| cap.action.is_details()) {
                assert_eq!(k, caps.len() - 1, "{}@{cols}: Details › is last", msg.title);
            }
            if cols >= 120 {
                assert!(
                    last.action.is_details(),
                    "{}@{cols}: a wide row keeps its Details ›",
                    msg.title
                );
            }
        }
    }
}

/// A moving comet leaves the glyph cell to the row's own glyph, on every
/// width and at every frame of a crossing (ruling 251): the comet already
/// says the work moves, and the painter draws that glyph as an icon.
#[test]
fn a_moving_comet_keeps_the_rows_own_glyph() {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(rows_with_buttons().remove(2), stamp(), now);
    c.commit_rows(now, 3);
    for cols in [60usize, 80, 120, 160] {
        let p = present(&c, cols);
        for k in 0..60u64 {
            let at = now + Duration::from_millis(33 * k + 1);
            let m = c.motion(&p, at, Look::MOVING);
            assert!(matches!(m.rows[0].anim, Anim::Comet { .. }), "{cols}/{k}");
            assert_eq!(m.rows[0].glyph, None, "{cols}/{k}: no spinner");
        }
    }
}
