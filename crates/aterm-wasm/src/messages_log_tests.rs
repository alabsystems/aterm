// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn log_reply_matches_original_projection_for_wrapped_and_saturated_cursors() {
    fn original(band: &WebBand, cursor: u64) -> String {
        let end = band.journal_first.saturating_add(band.journal.len() as u64);
        let from = cursor.min(end);
        let mut lines = Vec::new();
        if from < band.journal_first {
            let count = u32::try_from(band.journal_first - from).unwrap_or(u32::MAX);
            lines.push(LogLine::Dropped { count }.encode());
        }
        let skip = usize::try_from(from.saturating_sub(band.journal_first)).unwrap_or(usize::MAX);
        lines.extend(band.journal.iter().skip(skip).cloned());
        let mut out = format!("OK {end} {}", lines.len());
        for line in lines {
            out.push('\n');
            out.push_str(&line);
        }
        out
    }
    let mut band = WebBand::new(Instant::now(), 0, 0, 0);
    for rows in [0, 1, 64, JOURNAL_CAP] {
        band.journal = VecDeque::with_capacity(rows + 3);
        band.journal
            .extend((0..rows).map(|i| format!("entry{i} 漢🙂%20\t")));
        for i in 0..rows / 2 {
            band.journal.pop_front();
            band.journal.push_back(format!("wrapped{i} é%25"));
        }
        if rows >= 64 {
            assert!(
                !band.journal.as_slices().1.is_empty(),
                "wrapped storage control"
            );
        }
        for first in [0, 3, u64::from(u32::MAX) + 9, u64::MAX - 1] {
            band.journal_first = first;
            let end = first.saturating_add(rows as u64);
            for cursor in [
                0,
                first.saturating_sub(1),
                first,
                first.saturating_add(1),
                end.saturating_sub(1),
                end,
                end.saturating_add(1),
                u64::MAX,
            ] {
                assert_eq!(
                    band.log_since(cursor),
                    original(&band, cursor),
                    "rows={rows} first={first} cursor={cursor}"
                );
            }
        }
    }
}
