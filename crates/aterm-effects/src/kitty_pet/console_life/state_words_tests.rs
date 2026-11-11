// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::PetAttention;
use super::*;
use crate::pet_glyphs_gen::PetGlyphId;
use crate::state_words::{StateWords, Word};
use aterm_time::Duration;

fn words(c: &ConsoleLife, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    c.state_words(&mut w);
    w.into_words()
}

fn surface(session: u64, terminal_id: u64) -> PetSurface {
    PetSurface {
        session,
        terminal_id,
        alt_screen: false,
        rows: 24,
        cols: 80,
        absolute_row_revision: 1,
        history_renumber_epoch: 0,
        scroll_invalidation: 0,
    }
}

fn stamp(session: u64, terminal_id: u64) -> PetWorldStamp {
    PetWorldStamp {
        surface: surface(session, terminal_id),
        content_seq: 7,
        display_offset: 0,
        top_absolute_row: 3,
        uniform_up_rows: 0,
        scroll_band_batches: 0,
        scroll_band_seq: 0,
    }
}

/// A named change to a fresh console resident, given the script's `t0`.
type Case = (&'static str, fn(&mut ConsoleLife, Instant));

#[test]
fn each_console_episode_hold_seat_and_row_moves_its_words() {
    let t0 = Instant::now();
    let base = words(&ConsoleLife::new(), t0);
    assert_eq!(base, words(&ConsoleLife::new(), t0));
    let cases: [Case; 16] = [
        ("the input episode", |c, t0| {
            c.input = Some(InputEpisode {
                seq: 2,
                kind: PetInputKind::Text,
                at: t0 + Duration::from_millis(4),
                target: Some((3.0, 9.0)),
            });
        }),
        ("the attention", |c, _| c.attention = PetAttention::Typing),
        ("its reason", |c, _| c.reason = "typed"),
        ("the anchor", |c, _| {
            c.anchor = Some(PetAnchor {
                surface: surface(1, 1),
                block_id: 4,
                absolute_row: 12,
                row: 2.0,
                col: 0.0,
                state: BlockState::Executing,
                exit_code: None,
            });
        }),
        ("the target", |c, _| {
            c.target = Some(PetRect::new(2.0, 5.0, 1.0, 3.0));
        }),
        ("a completion", |c, t0| c.completion = Some((t0, true))),
        ("the pose", |c, _| c.pose = Some(PetGlyphId::PetSit)),
        ("the last observation's stamp", |c, _| {
            c.tick_stamp = Some(stamp(1, 1));
        }),
        ("the progress followed", |c, _| {
            c.progress = Some(TaskbarProgress::Normal(40));
        }),
        ("the perch trip", |c, _| {
            c.trip = Some(PerchTrip {
                from: (1.0, 2.0),
                to: (1.0, 9.0),
                elapsed: 0.1,
                duration: 0.4,
                hop: true,
            });
        }),
        ("the visibility hold", |c, t0| {
            c.hold.contrary_since = Some(t0 + Duration::from_millis(8));
        }),
        ("the z-order hold", |c, _| c.z.under = true),
        ("the motes it hid", |c, _| {
            c.mote_gate = MoteGate::Clearance;
            c.hidden_motes[1] = true;
        }),
        ("the rest seat", |c, _| c.rest_seat.set(Some((4.0, 2.0)))),
        ("the pause hold", |c, t0| {
            c.pause_hold_until
                .set(Some(t0 + Duration::from_millis(600)));
        }),
        ("a row's signature and change", |c, t0| {
            c.row_sig.push(0xABCD);
            c.row_changed.push(Some(t0));
        }),
    ];
    for (what, set) in cases {
        let mut c = ConsoleLife::new();
        set(&mut c, t0);
        assert_ne!(words(&c, t0), base, "{what} is hashed");
    }
}

#[test]
fn a_stamp_is_hashed_by_its_screen_and_epochs_not_its_session() {
    let t0 = Instant::now();
    let stamped = |s: PetWorldStamp| {
        let mut c = ConsoleLife::new();
        c.tick_stamp = Some(s);
        words(&c, t0)
    };
    let base = stamped(stamp(4, 4));
    assert_eq!(
        stamped(stamp(9, 12)),
        base,
        "a session's and a terminal's ids are the process's count of terminals, not state"
    );
    let mut scrolled = stamp(4, 4);
    scrolled.surface.scroll_invalidation = 1;
    assert_ne!(stamped(scrolled), base, "the surface's epochs are hashed");
    let mut moved = stamp(4, 4);
    moved.display_offset = 2;
    assert_ne!(
        stamped(moved),
        base,
        "the stamp's scroll position is hashed"
    );
}
