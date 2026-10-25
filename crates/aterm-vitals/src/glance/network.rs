// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Network's block (`docs/DESIGN-top-deck-merged-2026-10-21.md` §5, §9): the rate its header names
//! as 100%, and the swing its gauge's jagged top draws. Its score is the row's
//! [`ResourceView::heat_now`] and its run the row's [`ResourceView::heat_columns`], both the busier
//! direction's share of that direction's capacity.

use crate::model::{CapBasis, ResourceView};
use crate::ring::{BUCKET_S, columns_of};
use crate::words;

/// The buckets the swing looks back over: the last 20 s.
pub const SWING_BUCKETS: usize = 2;

/// What 100% means in Network's header, in bytes a second: the binding (busier) direction's
/// capacity while the basis is a rate, the link's driver rate for the first day and then the
/// learned 7-day peak. `None` while the row still learns its peak with no link to measure
/// against, or knows no rate.
#[must_use]
pub fn capacity(view: &ResourceView) -> Option<f64> {
    match view.basis {
        CapBasis::Link | CapBasis::Peak => view
            .binding()
            .capacity
            .filter(|c| c.is_finite() && *c > 0.0),
        CapBasis::Fixed | CapBasis::Learning => None,
    }
}

/// Network's capacity words in row A, [`capacity`] in bits ([`words::link_capacity`]):
/// `~1 Gb/s`. `None` (no words) where there is no capacity.
#[must_use]
pub fn capacity_words(view: &ResourceView) -> Option<String> {
    capacity(view).map(words::link_capacity)
}

/// How far the binding direction's single readings rose above their bucket's mean over the last
/// [`SWING_BUCKETS`] buckets before `now_unix_ms`, as a share of its capacity (`0..=1`): the
/// gauge's jagged top. Network keeps no per-reading spread (§4's MAD), only each 10 s bucket's
/// mean and largest reading, so this is the widest of those excursions. 0 where nothing was read
/// in that time or there is no [`capacity`].
#[must_use]
pub fn swing(view: &ResourceView, now_unix_ms: i64) -> f32 {
    let Some(cap) = capacity(view) else {
        return 0.0;
    };
    let (ring, per_s) = (view.binding().ring, BUCKET_S as u32);
    let widest = columns_of(&ring, view.ring_k, per_s, SWING_BUCKETS, now_unix_ms)
        .iter()
        .filter_map(|c| Some(c.max? - c.mean?))
        .filter(|d| d.is_finite())
        .fold(0.0, f64::max);
    (widest / cap).clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::Series;
    use crate::ring::{BUCKET_MS, Cell};

    const NOW: i64 = 1_792_000_800_000;
    const LINK: f64 = 125e6;

    fn cell(mean: f64, max: f64) -> Cell {
        Cell {
            mean: Some(mean),
            max: Some(max),
            over: false,
        }
    }

    /// A row of `rx` and `tx` cells (oldest first, the last the open bucket at `NOW`) and their
    /// capacities under `basis`, whose `now` is each ring's last mean.
    fn row(
        basis: CapBasis,
        rx: (&[Cell], Option<f64>),
        tx: (&[Cell], Option<f64>),
    ) -> ResourceView {
        let now = |cells: &[Cell]| cells.last().and_then(|c| c.mean);
        let share = |cells: &[Cell], cap: Option<f64>| Some(now(cells)? / cap?);
        let tx_binds = match (share(rx.0, rx.1), share(tx.0, tx.1)) {
            (Some(a), Some(b)) => b > a,
            (None, Some(_)) => true,
            _ => false,
        };
        ResourceView {
            now: now(rx.0),
            capacity: rx.1,
            ring: Arc::from(rx.0),
            ring_k: NOW.div_euclid(BUCKET_MS),
            basis,
            second: Some(Series {
                now: now(tx.0),
                capacity: tx.1,
                ring: Arc::from(tx.0),
                binds: tx_binds,
            }),
            ..ResourceView::default()
        }
    }

    #[test]
    fn the_header_names_the_binding_directions_rate_under_each_basis() {
        let quiet = [cell(1e6, 1e6); 3];
        let link = row(CapBasis::Link, (&quiet, Some(LINK)), (&quiet, Some(LINK)));
        assert_eq!(
            capacity_words(&link).as_deref(),
            Some("~1 Gb/s"),
            "the link's rate"
        );

        // Under the learned peaks each direction has its own; the busier one's share names it.
        let (down, up) = ([cell(15e6, 15e6); 3], [cell(2e6, 2e6); 3]);
        let rx_binds = row(CapBasis::Peak, (&down, Some(30e6)), (&up, Some(10e6)));
        assert_eq!(capacity(&rx_binds), Some(30e6));
        assert_eq!(capacity_words(&rx_binds).as_deref(), Some("~240 Mb/s"));
        let tx_binds = row(CapBasis::Peak, (&up, Some(30e6)), (&down, Some(20e6)));
        assert_eq!(
            capacity_words(&tx_binds).as_deref(),
            Some("~160 Mb/s"),
            "the upload binds at 75% of its peak, against 7% down"
        );

        let learning = row(CapBasis::Learning, (&quiet, None), (&quiet, None));
        assert_eq!(capacity_words(&learning), None, "nothing measures it yet");
        let unread = row(CapBasis::Peak, (&quiet, None), (&quiet, None));
        assert_eq!(
            capacity_words(&unread),
            None,
            "a peak with no rate says none"
        );
    }

    #[test]
    fn the_swing_is_the_widest_rise_over_a_buckets_mean_in_the_last_20_s() {
        let steady = [cell(15e6, 15e6), cell(15e6, 15e6), cell(15e6, 15e6)];
        let flat = row(CapBasis::Link, (&steady, Some(LINK)), (&steady, Some(LINK)));
        assert_eq!(swing(&flat, NOW), 0.0, "a steady transfer does not swing");

        // A burst 30 s ago is past the window; one in the open bucket and one before it count.
        let bursts = [cell(10e6, 90e6), cell(15e6, 32.5e6), cell(15e6, 20e6)];
        let quiet = [cell(1e6, 1e6); 3];
        let swung = row(CapBasis::Link, (&bursts, Some(LINK)), (&quiet, Some(LINK)));
        assert!(
            (swing(&swung, NOW) - 0.14).abs() < 1e-6,
            "17.5 MB/s over its mean is 14% of 125 MB/s: {}",
            swing(&swung, NOW)
        );

        let later = NOW + 3 * BUCKET_MS;
        assert_eq!(
            swing(&swung, later),
            0.0,
            "buckets 30 s old no longer swing"
        );
        let learning = row(CapBasis::Learning, (&bursts, None), (&quiet, None));
        assert_eq!(swing(&learning, NOW), 0.0, "no capacity, no share");
    }

    /// The mockups' scenes: busy swings from 12% up to 26% over the last 20 s and calm from 1% to
    /// 3%, both on the reference link; the absent scene names no rate and does not swing.
    #[test]
    fn the_scenes_swing_as_the_mockups_draw_them() {
        use crate::fixtures::{self, DeckScene};
        use crate::model::Resource;

        let read = |scene| {
            let snap = fixtures::snapshot(scene);
            let view = &snap.resources[Resource::Network.index()];
            (swing(view, snap.at_unix_ms), capacity_words(view))
        };
        let (busy, calm, absent) = (
            read(DeckScene::Busy),
            read(DeckScene::Calm),
            read(DeckScene::Absent),
        );
        assert!((busy.0 - 0.14).abs() < 1e-3, "busy: {busy:?}");
        assert!((calm.0 - 0.02).abs() < 1e-3, "calm: {calm:?}");
        assert_eq!(busy.1.as_deref(), Some("~1 Gb/s"));
        assert_eq!(calm.1.as_deref(), Some("~1 Gb/s"));
        assert_eq!(absent, (0.0, None));
    }
}
