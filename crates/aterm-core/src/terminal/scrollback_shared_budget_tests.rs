// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn with_clean_globals(f: impl FnOnce()) {
    let _guard = SERIAL.lock().expect("serial lock");
    set_global_scrollback_budget(0);
    assert_eq!(
        LIVE_SHARES.load(Ordering::Relaxed),
        0,
        "test left a share registered"
    );
    f();
    set_global_scrollback_budget(0);
}

#[test]
fn unset_global_passes_configured_through() {
    with_clean_globals(|| {
        let mut share = ScrollbackBudgetShare::register(64);
        assert_eq!(share.pending_effective(), Some(64), "first poll applies");
        assert_eq!(share.pending_effective(), None, "second poll is settled");
    });
}

#[test]
fn reconfigure_and_global_change_surface_on_next_poll() {
    with_clean_globals(|| {
        let mut share = ScrollbackBudgetShare::register(500);
        assert_eq!(share.pending_effective(), Some(500));
        set_global_scrollback_budget(200);
        assert_eq!(share.pending_effective(), Some(200));
        share.set_configured(120);
        assert_eq!(share.pending_effective(), Some(120));
        share.set_configured(0);
        assert_eq!(share.pending_effective(), Some(1));
    });
}
