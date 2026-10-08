// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, WindowId, term_lock};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

type Tally = BTreeMap<&'static str, u64>;

fn tally() -> Tally {
    crate::metrics::frame_refill_full_causes()
        .into_iter()
        .map(|c| (c.cause, c.frames))
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
    let before = tally();
    let m0 = crate::metrics::snapshot();
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
    let m1 = crate::metrics::snapshot();
    Arms {
        // Saturating for the same reason `since` is.
        scoped: m1
            .frame_refills_scoped
            .saturating_sub(m0.frame_refills_scoped),
        full: m1.frame_refills_full.saturating_sub(m0.frame_refills_full),
        causes: since(&before),
        painted,
    }
}

/// A settled split of two or three panes books one scoped extraction per
/// leaf per painted frame (background and focused) and nothing to the full
/// arm. Assertions are lower bounds that survive concurrent tests.
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
        // One scoped extraction per leaf per painted frame; a lower bound,
        // since concurrent tests only add and frames can exceed `FRAMES - 2`.
        let leaves = panes as u64;
        assert!(
            d.scoped >= (FRAMES as u64 - 2) * leaves,
            "a settled split must keep every leaf — focused included — on the scoped \
             arm: {d:?}"
        );
        for fatal in ["terminal_mismatch", "scratch_row_count", "row_shift"] {
            assert_eq!(
                d.causes.get(fatal).copied().unwrap_or(0),
                0,
                "a settled {panes}-pane split must not refuse under `{fatal}`: {d:?}"
            );
        }
    }
}

/// The composed focused leaf stays on the DMG-1 carrier. A regression is
/// silent (no other counter moves), so the failure names the refusing
/// clause: `host_mutation` means a pass-2 mutator skipped the `snapshot_seq`
/// bump, `damage_taken` means a bare `take_damage` bumped the generation
/// without restamping, `terminal_mismatch` means the focused pane lost its
/// dedicated buffer.
#[test]
fn the_composed_focused_leaf_stays_on_the_carrier() {
    use aterm_core::render::FrameRefill;

    let (mut app, wid, sids) = split_window(2);
    let focus = *sids.last().expect("the newest pane takes focus");
    let _ = steady_state(&mut app, wid, focus, 8);

    let term = app.pool.get(focus).expect("pane session").term.clone();
    let ws = app.windows.get_mut(&wid).expect("window");
    let (rows, cols) = (
        ws.composed_focus_scratch.rows,
        ws.composed_focus_scratch.cols,
    );
    assert!(rows > 0 && cols > 0, "the focused leaf was extracted");
    // Offer the buffer just filled straight back to the carrier: a healthy
    // chain retains every row. Touches no global counter.
    let settled =
        term_lock(&term).cell_frame_damage_scoped_into(&mut ws.composed_focus_scratch, rows, cols);
    crate::logging::stderr_line!("SPLIT focused-leaf carrier state, settled: {settled:?}");
    assert!(
        matches!(settled, FrameRefill::Scoped { .. }),
        "the composed focused leaf must arrive with its continuity chain intact — it \
         refused under `{}`. That clause NAMES the repair: `host_mutation` = a pass-2 \
         host mutator wrote the scratch without bumping `snapshot_seq`; `damage_taken` \
         = the historical `cell_frame_into` + bare `take_damage` pair is back; \
         `terminal_mismatch` = the focused pane no longer has its own buffer",
        match settled {
            FrameRefill::Full { cause } => cause.as_str(),
            FrameRefill::Scoped { .. } => "n/a",
        }
    );

    // The chain is live: one more echo is served by re-resolving a couple
    // of rows, not the viewport.
    term_lock(&term).process(b"\rlive");
    let echoed =
        term_lock(&term).cell_frame_damage_scoped_into(&mut ws.composed_focus_scratch, rows, cols);
    crate::logging::stderr_line!("SPLIT focused-leaf carrier state, one echo: {echoed:?}");
    let refilled = match echoed {
        FrameRefill::Scoped { rows_refilled } => rows_refilled,
        FrameRefill::Full { cause } => panic!(
            "a one-row echo into the focused pane refused the scoped arm under `{}`",
            cause.as_str()
        ),
    };
    assert!(
        refilled >= 1 && refilled < rows,
        "a one-row echo must re-resolve a few rows, not the viewport: {refilled} of {rows}"
    );
}
