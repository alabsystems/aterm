// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{FAST_SEAM, Timings};

/// The fast clock keeps every ORDER the production one has between its
/// waits — a cold restore's wait outlasts the ended-at-once count, a look
/// is shorter than that count — and is shorter or equal everywhere; and
/// the seam is denied to every child shell, by this very name.
#[test]
fn the_fast_clock_keeps_the_production_orders_and_its_seam_is_never_inherited() {
    for t in [Timings::PRODUCTION, Timings::FAST] {
        assert!(t.restored_held > t.ended_at_once, "{t:?}");
        assert!(t.shell_look < t.ended_at_once, "{t:?}");
        assert!(t.restored_held < t.held, "{t:?}");
    }
    let (p, f) = (Timings::PRODUCTION, Timings::FAST);
    assert!(
        f.claim_ttl > aterm_spec::wait::MUST_HAPPEN,
        "a lease never lapses inside the hang detector a release is waited for under"
    );
    for (prod, fast) in [
        (p.exited, f.exited),
        (p.prompt_back, f.prompt_back),
        (p.prompt_drawn, f.prompt_drawn),
        (p.fence_every, f.fence_every),
        (p.held, f.held),
        (p.restored_held, f.restored_held),
        (p.ended_at_once, f.ended_at_once),
        (p.shell_look, f.shell_look),
        (p.first_idle, f.first_idle),
        (p.alab_first_idle, f.alab_first_idle),
        (p.carry_settle, f.carry_settle),
        (p.muse_found, f.muse_found),
        (p.muse_up, f.muse_up),
        (p.muse_lock, f.muse_lock),
        (p.muse_adopt, f.muse_adopt),
        (p.agy_found, f.agy_found),
        (p.agy_up, f.agy_up),
        (p.agy_lock, f.agy_lock),
        (p.agy_adopt, f.agy_adopt),
        (p.redecide_after_skip, f.redecide_after_skip),
        (p.drive_reply, f.drive_reply),
        (p.owner_word_wait, f.owner_word_wait),
    ] {
        assert!(fast <= prod, "{fast:?} > {prod:?}");
    }
    assert!(aterm_types::domain::is_ai_env_var(FAST_SEAM));
}
