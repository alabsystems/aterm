// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tiered `Scrollback` over exhaustive small ranges and generated lines:
//! order and content survive hot -> warm -> cold promotion, forward and
//! reverse reads agree, and the line limit evicts oldest-first.

use super::*;

/// Content survives the hot -> warm -> cold transitions, and every line is
/// found with its marker in whichever tier it landed.
#[test]
fn scrollback_tier_content_preserved() {
    for hot_lines in 3usize..10 {
        for warm_lines in 5usize..20 {
            let mut sb = Scrollback::new(hot_lines, warm_lines, 1_000_000);
            let total_lines = hot_lines + warm_lines + 5;
            for i in 0..total_lines {
                sb.push_str(&format!("MARKER_{i:03}_DATA"));
            }
            for i in 0..total_lines {
                let line = sb.get_line(i).unwrap();
                assert_eq!(
                    line.map(|l| l.to_string()),
                    Some(format!("MARKER_{i:03}_DATA")),
                    "line {i} (hot {hot_lines}, warm {warm_lines})"
                );
            }
            assert_eq!(sb.line_count(), total_lines);
        }
    }
}

/// FIFO ordering: `get_line(i)` is the i-th oldest line and `get_line_rev(i)`
/// the i-th newest.
#[test]
fn scrollback_fifo_ordering() {
    let mut rng = aterm_spec::rng::XorShift::new(0xF1F0_0F1F);
    let mut next = move |n: u64| rng.below(n);
    for _ in 0..256 {
        let lines: Vec<String> = (0..1 + next(49))
            .map(|_| {
                (0..1 + next(10))
                    .map(|_| char::from(b'a' + next(26) as u8))
                    .collect()
            })
            .collect();
        let mut sb = Scrollback::new(100, 1000, 10_000_000);
        for line_str in &lines {
            sb.push_str(line_str);
        }
        assert_eq!(sb.line_count(), lines.len());
        for (i, expected) in lines.iter().enumerate() {
            let text = sb.get_line(i).unwrap().expect("in range");
            assert_eq!(
                text.as_str().unwrap_or(""),
                expected.as_str(),
                "get_line({i})"
            );
        }
        for (rev_i, expected) in lines.iter().rev().enumerate() {
            let text = sb.get_line_rev(rev_i).unwrap().expect("in range");
            assert_eq!(
                text.as_str().unwrap_or(""),
                expected.as_str(),
                "get_line_rev({rev_i})"
            );
        }
    }
}

/// The line limit is enforced, and eviction keeps the most recent lines.
#[test]
fn scrollback_line_limit_enforced() {
    for limit in 1usize..20 {
        for push_count in 1usize..50 {
            let mut sb = Scrollback::new(100, 1000, 10_000_000);
            sb.set_line_limit(Some(limit));
            for i in 0..push_count {
                sb.push_str(&format!("line{i}"));
            }
            assert!(
                sb.line_count() <= limit,
                "limit {limit}, pushed {push_count}"
            );
            if push_count > limit {
                let text = sb.get_line(0).unwrap().expect("in range");
                assert_eq!(
                    text.as_str().unwrap_or(""),
                    format!("line{}", push_count - limit),
                    "limit {limit}, pushed {push_count}"
                );
            }
        }
    }
}
