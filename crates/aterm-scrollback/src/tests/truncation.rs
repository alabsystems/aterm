// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Truncate and remove_newest operations including cross-tier slow paths.

use super::*;

/// A hot=5 / warm=10 / block=5 scrollback holding `L-00..L-29`, so the newest
/// lines are in hot, the middle in warm and the oldest in cold.
fn three_tier() -> Scrollback {
    let mut sb = Scrollback::with_block_size(5, 10, 10_000_000, 5);
    for i in 0..30 {
        sb.push_str(&format!("L-{i:02}"));
    }
    assert!(sb.cold_line_count() > 0 && sb.warm_line_count() > 0);
    sb
}

/// `truncate` keeps exactly the newest `keep` lines on every path — hot only,
/// into warm, into cold (#1693: it once cleared cold/warm whenever `keep`
/// exceeded the hot tier) and zero — and never leaves hot over its limit.
#[test]
fn truncate_keeps_the_newest_lines_on_every_path() {
    let (hot, warm) = {
        let sb = three_tier();
        (sb.hot_line_count(), sb.warm_line_count())
    };
    for keep in [
        hot.saturating_sub(1),
        hot + 2,
        hot + warm,
        hot + warm + 5,
        0,
    ] {
        let mut sb = three_tier();
        sb.truncate(keep).expect("truncate should succeed");
        assert_eq!(sb.line_count(), keep, "keep {keep}");
        assert!(sb.hot_line_count() <= sb.hot_limit(), "keep {keep}");
        for i in 0..keep {
            let line = sb.get_line(i).expect("no error").expect("line present");
            assert_eq!(line.to_string(), format!("L-{:02}", 30 - keep + i));
        }
    }
    let mut sb = three_tier();
    sb.truncate(0).expect("truncate should succeed");
    assert_eq!(
        (
            sb.hot_line_count(),
            sb.warm_line_count(),
            sb.cold_line_count()
        ),
        (0, 0, 0)
    );
}

/// `remove_newest` keeps exactly the oldest lines on every path — the hot
/// fast path, a partial warm block, past warm into cold, and everything. The
/// slow path once dumped every survivor into hot past `hot_limit`; it
/// re-promotes instead.
#[test]
fn remove_newest_keeps_the_oldest_lines_on_every_path() {
    let (hot, warm) = {
        let sb = three_tier();
        (sb.hot_line_count(), sb.warm_line_count())
    };
    for remove in [2, hot + 1, hot + 2, hot + warm + 3, 30] {
        let mut sb = three_tier();
        sb.remove_newest(remove)
            .expect("remove_newest should succeed");
        let expected = 30 - remove;
        assert_eq!(sb.line_count(), expected, "remove {remove}");
        assert!(sb.hot_line_count() <= sb.hot_limit(), "remove {remove}");
        for i in 0..expected {
            let line = sb.get_line(i).expect("no error").expect("line present");
            assert_eq!(line.to_string(), format!("L-{i:02}"), "remove {remove}");
        }
    }
    let mut sb = three_tier();
    sb.remove_newest(30).expect("remove everything");
    assert_eq!(
        (
            sb.hot_line_count(),
            sb.warm_line_count(),
            sb.cold_line_count()
        ),
        (0, 0, 0)
    );
}
