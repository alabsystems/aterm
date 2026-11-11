// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{HistoryEpoch, ViewportRowCache};
use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, Line};
use std::sync::Arc;

#[test]
fn a_hyperlink_fact_requires_the_same_epoch_and_absolute_row() {
    let cache = ViewportRowCache::default();
    let epoch = HistoryEpoch {
        content_gen: 1,
        renumber: 2,
        cols: 8,
        visible_rows: 3,
    };
    assert_eq!(
        cache.hyperlink_presence(epoch, 10),
        None,
        "unknown is not absence"
    );
    let plain = Line::with_hyperlinks("plain", Rle::<CellAttrs>::new(), vec![]);
    let plain = Arc::new(crate::materialize_from_line(&plain, 8));
    cache.store(epoch, 10, &plain);
    assert_eq!(cache.hyperlink_presence(epoch, 10), Some(false));
    assert_eq!(
        cache.hyperlink_presence(epoch, 18),
        None,
        "same slot, different row"
    );
    for changed in [
        HistoryEpoch {
            content_gen: 2,
            ..epoch
        },
        HistoryEpoch {
            renumber: 3,
            ..epoch
        },
        HistoryEpoch { cols: 9, ..epoch },
        HistoryEpoch {
            visible_rows: 4,
            ..epoch
        },
    ] {
        assert_eq!(cache.hyperlink_presence(changed, 10), None);
    }
    let linked = Line::with_hyperlinks(
        "link",
        Rle::new(),
        vec![HyperlinkSpan::new(
            0,
            4,
            Arc::from("https://cache.example/"),
        )],
    );
    let linked = Arc::new(crate::materialize_from_line(&linked, 8));
    cache.store(epoch, 18, &linked);
    assert_eq!(cache.hyperlink_presence(epoch, 18), Some(true));
    assert_eq!(cache.hyperlink_presence(epoch, 10), None);
}
