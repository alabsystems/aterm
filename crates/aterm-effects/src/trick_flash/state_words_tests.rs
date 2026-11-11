// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::state_words::{StateWords, Word};
use aterm_time::{Duration, Instant};

fn words(t: &TrickFlash, t0: Instant) -> Vec<Word> {
    let mut w = StateWords::new(t0);
    t.state_words(&mut w);
    w.into_words()
}

#[test]
fn a_noted_word_its_revoke_and_a_declaration_move_the_flash_words() {
    let t0 = Instant::now();
    let base = words(&TrickFlash::new(), t0);
    assert_eq!(
        base,
        words(&TrickFlash::new(), t0),
        "an idle flash's words are fixed"
    );

    let note = |pane| {
        let mut flash = TrickFlash::new();
        flash.note(t0, Some(pane), Some(pane), Some((2, 5)), "sit", 0);
        flash
    };
    let noted = words(&note(38), t0);
    assert_ne!(noted, base, "a noted word is hashed");
    assert_eq!(
        words(&note(41), t0),
        noted,
        "a pane's id is the process's count of terminals, not state"
    );
    let mut other = TrickFlash::new();
    other.note(t0, Some(38), Some(38), Some((2, 5)), "down", 0);
    assert_ne!(
        words(&other, t0),
        noted,
        "the noted word's scalars are hashed"
    );
    let mut revoked = note(38);
    if let Some(slot) = revoked.slot.as_mut() {
        slot.revoked_at = Some(t0 + Duration::from_millis(40));
    }
    assert_ne!(words(&revoked, t0), noted, "a revoke's instant is hashed");
    let mut declared = TrickFlash::new();
    declared.declare_base_y(Some(38), Some(12));
    assert_ne!(
        words(&declared, t0),
        base,
        "the host's declaration is hashed"
    );
}
