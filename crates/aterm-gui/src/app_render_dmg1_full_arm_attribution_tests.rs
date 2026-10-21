// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, WindowId, term_lock};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

type Tally = BTreeMap<&'static str, u64>;

fn tally() -> Tally {
    crate::metrics::METRICS
        .window
        .redraw
        .refill_cause
        .sparse(
            aterm_core::render::FullRefillCause::ALL
                .iter()
                .map(|cause| (cause.index(), cause.as_str())),
        )
        .into_iter()
        .collect()
}

/// The per-cause delta since `before`, saturating so a concurrent
/// `metrics reset` yields a smaller number instead of an overflow panic.
fn since(before: &Tally) -> Tally {
    tally()
        .into_iter()
        .filter_map(|(cause, now)| {
            let d = now.saturating_sub(before.get(cause).copied().unwrap_or(0));
            (d != 0).then_some((cause, d))
        })
        .collect()
}

/// A focused window with `panes` stub panes in its active tab, each seeded
/// and settled (a fresh terminal's `Damage::Full` would report `full_damage`).
fn split_window(panes: usize) -> (App, WindowId, Vec<u64>) {
    assert!(panes >= 2, "a split has at least two panes");
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut sids = vec![0u64];
    for _ in 1..panes {
        sids.push(app.split_active_stub_tab(wid));
    }
    app.windows.get_mut(&wid).expect("window").focused = true;
    for sid in &sids {
        let term = app.pool.get(*sid).expect("pane session").term.clone();
        let mut t = term_lock(&term);
        t.process(b"pane seed\r");
        t.take_damage();
    }
    (app, wid, sids)
}

/// The arms and their attribution over one workload.
struct Arms {
    scoped: u64,
    full: u64,
    causes: Tally,
    /// Composed presents that repainted (an early-out extracts nothing, so
    /// zero here would make the tally vacuous).
    painted: usize,
}

impl std::fmt::Debug for Arms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "painted={} scoped={} full={} causes={:?}",
            self.painted, self.scoped, self.full, self.causes
        )
    }
}

/// `frames` composed presents, echoing one alternating glyph into `into`
/// before each. Returns the arm split and the per-cause delta.
fn steady_state(app: &mut App, wid: WindowId, into: u64, frames: usize) -> Arms {
    let term = app.pool.get(into).expect("pane session").term.clone();
    let mut now = Instant::now();
    let redraw = &crate::metrics::METRICS.window.redraw;
    let before = tally();
    let (scoped0, full0) = (redraw.refills_scoped.get(), redraw.refills_full.get());
    let mut painted = 0usize;
    for i in 0..frames {
        term_lock(&term).process(if i % 2 == 0 { b"\rx" } else { b"\ry" });
        now += Duration::from_millis(16);
        if app
            .redraw_compose(wid, 24, 80, false, false, None, 0, now)
            .is_some()
        {
            painted += 1;
        }
    }
    Arms {
        // Saturating for the same reason `since` is.
        scoped: redraw.refills_scoped.get().saturating_sub(scoped0),
        full: redraw.refills_full.get().saturating_sub(full0),
        causes: since(&before),
        painted,
    }
}

/// A settled split of two or three panes books one scoped extraction per leaf per painted frame
/// (focused included) and nothing to the full arm. Assertions are lower bounds: concurrent tests
/// only add.
#[test]
fn a_split_window_keeps_every_leaf_on_the_scoped_arm() {
    for panes in [2usize, 3] {
        let (mut app, wid, sids) = split_window(panes);
        let focus = *sids.last().expect("the newest pane takes focus");
        // Warm: the first composed frames fill each pane scratch from
        // nothing (`scratch_unstamped`), which is not the steady state.
        let _ = steady_state(&mut app, wid, focus, 4);
        const FRAMES: usize = 32;
        let d = steady_state(&mut app, wid, focus, FRAMES);
        crate::logging::stderr_line!(
            "SPLIT({panes}) steady state (exact only under --exact): {d:?}"
        );
        assert!(
            d.painted >= FRAMES - 2,
            "the workload must actually present: {d:?}"
        );
        let leaves = panes as u64;
        assert!(
            d.scoped >= (FRAMES as u64 - 2) * leaves,
            "a settled split must keep every leaf — focused included — on the scoped \
             arm: {d:?}"
        );
    }
}
