// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_core::terminal::Terminal;

use super::{CastCuts, CastRecorder, ResizeStamp};

/// Simulates a reader `term_lock` hold processing `bytes`.
pub(crate) fn hold(term: &Mutex<Terminal>, cast: &Mutex<CastRecorder>, bytes: &[u8]) -> CastCuts {
    let watermark = cast.lock().unwrap().watermark();
    let mut cuts = CastCuts::default();
    let mut t = crate::term_lock(term);
    cuts.observe(0, &t, &watermark);
    t.process(bytes);
    cuts
}

/// Simulates a reader hold followed by recording `bytes` at `t`.
pub(crate) fn burst(term: &Mutex<Terminal>, cast: &Mutex<CastRecorder>, t: Duration, bytes: &[u8]) {
    let cuts = hold(term, cast, bytes);
    cast.lock().unwrap().record_burst(Arc::from(bytes), cuts, t);
}

/// Simulates a resize `term_lock` hold resizing `term` to `cols`×`rows` at `t`.
pub(crate) fn resize_hold(
    term: &Mutex<Terminal>,
    cast: &Mutex<CastRecorder>,
    t: Duration,
    rows: u16,
    cols: u16,
) -> ResizeStamp {
    let at = cast.lock().unwrap().epoch() + t;
    let mut term = crate::term_lock(term);
    term.resize(rows, cols);
    ResizeStamp::take_at(&term, at)
}

/// Simulates a resize hold followed by recording the stamped resize.
pub(crate) fn resize(
    term: &Mutex<Terminal>,
    cast: &Mutex<CastRecorder>,
    t: Duration,
    rows: u16,
    cols: u16,
) {
    let stamp = resize_hold(term, cast, t, rows, cols);
    cast.lock()
        .unwrap()
        .record_resize_stamped(stamp, cols, rows);
}
