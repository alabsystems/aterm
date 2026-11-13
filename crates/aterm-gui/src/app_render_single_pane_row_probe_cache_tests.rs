// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::SinglePaneRowProbeCache;
use aterm_core::terminal::Terminal;

fn sample(
    cache: &mut SinglePaneRowProbeCache,
    term: &Terminal,
    row: usize,
    host_rows: usize,
    host_cols: usize,
    v2: bool,
    probes: &mut [Vec<char>; 3],
) -> (bool, bool, bool) {
    let [caret, above, below] = probes;
    cache.sample(term, row, host_rows, host_cols, v2, (caret, above, below))
}

#[test]
fn the_row_probe_cache_restores_on_a_hit_and_every_key_term_breaks_reuse() {
    let mut term = Terminal::new(4, 12);
    term.process(b"above\r\ncaret\r\nbelow");
    let mut cache = SinglePaneRowProbeCache::default();
    let mut probes = [Vec::new(), Vec::new(), Vec::new()];
    let rows = |probes: &[Vec<char>; 3]| {
        probes
            .iter()
            .map(|p| p.iter().take(5).collect::<String>())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        sample(&mut cache, &term, 1, 4, 12, true, &mut probes),
        (false, true, true)
    );
    assert_eq!(rows(&probes), ["caret", "above", "below"]);
    for probe in &mut probes {
        probe.clear();
        probe.extend(['X'; 12]);
    }
    assert_eq!(
        sample(&mut cache, &term, 1, 4, 12, true, &mut probes),
        (true, true, true)
    );
    assert_eq!(
        rows(&probes),
        ["caret", "above", "below"],
        "restored on a hit"
    );

    let mut hit = |term: &Terminal, row, host: (usize, usize), v2| {
        sample(&mut cache, term, row, host.0, host.1, v2, &mut probes).0
    };
    assert!(!hit(&term, 2, (4, 12), true), "a different cursor row");
    assert!(!hit(&term, 2, (4, 13), true), "host geometry");
    assert!(!hit(&term, 2, (4, 13), false), "neighbor ownership");
    assert!(hit(&term, 2, (4, 13), false), "control: unchanged");
    term.process(b"Z");
    assert!(!hit(&term, 2, (4, 13), false), "new content");
    term.resize(5, 13);
    assert!(!hit(&term, 2, (5, 13), false), "a resize");
    term.process(b"\r\none\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
    assert!(!hit(&term, 1, (5, 13), false));
    term.scroll_display(1);
    assert!(term.grid().display_offset() > 0);
    assert!(!hit(&term, 1, (5, 13), false), "a different viewport");
    let other = Terminal::new(5, 13);
    assert!(
        !hit(&other, 1, (5, 13), false),
        "another terminal (a tab switch)"
    );
}
