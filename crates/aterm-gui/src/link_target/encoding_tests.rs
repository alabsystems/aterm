// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frozen cfc23fe58410 formatter and complete row, with borrowed runs and host shape.

use super::caption_test_support::instructions;
use super::*;

const PREVIOUS_HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];

/// Re-spell `text` so that nothing in it can reorder, hide inside, or
/// impersonate the rest of the line: printable ASCII survives verbatim, every
/// other character becomes the percent-encoding of its UTF-8 bytes.
fn previous_safe_form(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if previous_safe_width(ch) == 1 {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(PREVIOUS_HEX[usize::from(b >> 4)]);
                out.push(PREVIOUS_HEX[usize::from(b & 0x0f)]);
            }
        }
    }
    out
}

/// How many characters `ch` takes in the [`previous_safe_form`]: one for printable
/// ASCII, three (`%XX`) for each UTF-8 byte of anything else.
const fn previous_safe_width(ch: char) -> usize {
    if matches!(ch, '\u{21}'..='\u{7e}') {
        1
    } else {
        3 * ch.len_utf8()
    }
}

/// The byte range of the host inside a URL: the run the band refuses to elide. Every delimiter
/// is ASCII, which never occurs inside a multi-byte UTF-8 sequence, so it reads the raw URL;
/// [`caption`] converts the result to safe-form characters.
fn previous_host_span(url: &str) -> Range<usize> {
    let Some(colon) = url.find(':') else {
        // no scheme: the whole thing is the identity
        return 0..url.len();
    };
    let after = &url[colon + 1..];
    let special = ["http", "https", "ws", "wss", "ftp"]
        .iter()
        .any(|s| url[..colon].eq_ignore_ascii_case(s));
    let (authority, ends): (Option<&str>, &[char]) = if special {
        (
            Some(after.trim_start_matches(['/', '\\'])),
            &['/', '\\', '?', '#'],
        )
    } else {
        (after.strip_prefix("//"), &['/', '?', '#'])
    };
    if let Some(authority) = authority {
        let start = url.len() - authority.len();
        let end = start + authority.find(ends).unwrap_or(authority.len());
        let host_start = url[start..end]
            .rfind('@')
            .map_or(start, |at| start + at + 1);
        host_start..end
    } else {
        let end = colon + 1 + after.find(['?', '#']).unwrap_or(after.len());
        colon + 1..end
    }
}

fn previous_caption(url: &str, max_cells: usize) -> Caption {
    let raw = url.trim();
    let mut read = raw.len().min(SCAN_BYTES);
    while !raw.is_char_boundary(read) {
        read -= 1;
    }
    let span = previous_host_span(&raw[..read]);
    let shape = if raw[span.clone()].starts_with('[') {
        HostShape::Bracketed
    } else {
        HostShape::Labels
    };
    let finish = |text, host| Caption { text, host, shape };
    let known = !span.is_empty() && (span.end < read || read == raw.len());
    let width = |text: &str| text.chars().map(previous_safe_width).sum::<usize>();
    let host_start = width(&raw[..span.start]);
    let host = host_start..host_start + width(&raw[span.clone()]);
    let head_end = raw
        .char_indices()
        .nth(max_cells.saturating_add(1))
        .map_or(raw.len(), |(at, _)| at);
    let head = previous_safe_form(&raw[..head_end]);
    if head_end == raw.len() && head.len() <= max_cells {
        return finish(head, host);
    }
    match max_cells {
        0 => finish(String::new(), 0..0),
        1 => finish(ELLIPSIS.to_string(), 0..0),
        max if !known => {
            let mut text = raw[..read]
                .split_once(':')
                .map(|(scheme, _)| scheme)
                .filter(|scheme| {
                    !scheme.is_empty()
                        && scheme.len() + 2 <= max
                        && scheme
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-'))
                })
                .map_or(String::new(), |scheme| format!("{scheme}:"));
            text.push(ELLIPSIS);
            finish(text, 0..0)
        }
        max if host.end < max => {
            let mut text = head[..max - 1].to_string();
            text.push(ELLIPSIS);
            finish(text, host)
        }
        max => {
            let after_host = span.end < raw.len();
            let keep = if after_host { max - 2 } else { max - 1 };
            let from = match keep.checked_sub(1) {
                Some(back) => raw[..span.end]
                    .char_indices()
                    .rev()
                    .nth(back)
                    .map_or(0, |(at, _)| at),
                None => span.end,
            };
            let tail = previous_safe_form(&raw[from..span.end]);
            let start = host.end - keep;
            let mut text = String::with_capacity(max);
            text.push(ELLIPSIS);
            text.push_str(&tail[tail.len() - keep..]);
            if after_host {
                text.push(ELLIPSIS);
            }
            let host = 1 + host.start.max(start) - start..1 + keep;
            finish(text, host)
        }
    }
}

fn previous_row(
    url: &str,
    cols: usize,
    palette: impl Into<chrome_band::ChromePalette>,
    seam: bool,
    tracking: bool,
) -> Option<Vec<RenderCell>> {
    let body = cols.saturating_sub(MARGIN * 2);
    if body < MIN_URL_CELLS {
        return None;
    }
    let c = chrome_band::band_colors(palette);
    let mut row = blank_row(cols, c.label, c.bar_bg, seam);
    let lead = if !crate::is_safe_url(url) {
        BLOCKED
    } else if tracking {
        OPENS_TRACKED
    } else {
        OPENS
    };
    let with_lead = body.saturating_sub(lead.len() + GAP);
    let (url_col, url_cells) = if with_lead >= LEAD_IN_URL_CELLS {
        write_str(&mut row, cols, MARGIN, lead, c.label, c.bar_bg, false);
        (MARGIN + lead.len() + GAP, with_lead)
    } else {
        (MARGIN, body)
    };
    let cap = previous_caption(url, url_cells);
    if !cap.text.chars().any(|ch| ch != ELLIPSIS) {
        return None;
    }
    write_str(&mut row, cols, url_col, &cap.text, c.label, c.bar_bg, false);
    let emphasis = cap.emphasis();
    if !emphasis.is_empty() {
        write_str(
            &mut row,
            cols,
            url_col + emphasis.start,
            cap.slice(&emphasis),
            c.value,
            c.bar_bg,
            true,
        );
    }
    if seam {
        chrome_band::seal_band_top(&mut row, c.seam);
    }
    Some(row)
}

fn inputs() -> Vec<String> {
    let mut cases: Vec<String> = [
        "",
        "  ",
        "猫",
        "plain.example",
        "https:",
        "https://",
        "https://evil.example/x",
        "https://google.com@evil.co.uk/x",
        "https://gооgle.com.猫.example/🦀",
        "https://a\u{202e}b.example/path",
        "https://evil.example/a b",
        "https://evil.example/a\tb",
        "https://evil.example/a\0b",
        "https://evil.example/a\x7fb",
        "https:///evil.example/path",
        "https:\\\\evil.example\\path",
        "mailto:ops@evil.example?body=猫",
        "custom:opaque?x=1",
        "file:///tmp/a b",
        "  https://evil.example/x  ",
        "https://[2001:db8::ffff:192.0.2.1]:8443/x",
        "https://google.com@[::ffff:192.0.2.1%25en0.100]:8443/x",
        "https://[v1.a.b.c]:8443/x",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for len in 0..64 {
        cases.push(format!(
            "https://{}.猫.example/{}",
            "a".repeat(len),
            "🦀".repeat(len)
        ));
        cases.push(format!(
            "https://{}example.co.uk/{}",
            "a.".repeat(len),
            "x".repeat(len)
        ));
    }
    for len in [511, 1024, SCAN_BYTES - 1, SCAN_BYTES, SCAN_BYTES + 1] {
        cases.push(format!("https://{}.evil.example/path", "a".repeat(len)));
        cases.push(format!("https://{}猫.evil.example/path", "a".repeat(len)));
    }
    cases
}

#[test]
fn encoded_text_host_coordinates_and_shape_match_the_frozen_formatter() {
    for url in inputs() {
        for width in (0..=160).chain([256, usize::MAX]) {
            let cap = caption(&url, width);
            assert_eq!(
                cap,
                previous_caption(&url, width),
                "url={url:?} width={width}"
            );
            assert!(
                cap.text
                    .chars()
                    .all(|ch| ch.is_ascii_graphic() || ch == ELLIPSIS)
            );
        }
    }
}

#[test]
fn full_rows_match_with_current_borrowed_emphasis_and_literal_identity() {
    for url in inputs().iter().step_by(5) {
        for cols in [0, 8, 11, 12, 16, 20, 31, 40, 50, 80, 160] {
            for seam in [false, true] {
                for tracking in [false, true] {
                    for theme in [
                        Theme::default(),
                        Theme {
                            fg: 0x202020,
                            bg: 0xf0f0f0,
                            cursor: 0x008000,
                            selection: 0xc0c0e0,
                        },
                    ] {
                        assert_eq!(
                            caption_row(url, cols, theme, seam, tracking),
                            previous_row(url, cols, theme, seam, tracking),
                            "url={url:?} cols={cols} seam={seam} tracking={tracking}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual paired current-baseline formatter/row and safety-scan work measurement"]
fn url_formatter_cost() {
    use std::hint::black_box;
    type Formatter = fn(&str, usize) -> Caption;
    type Row = fn(&str, usize, Theme, bool, bool) -> Option<Vec<RenderCell>>;
    let host = format!("https://{}.evil.example/x", "a".repeat(400));
    let labels = format!("https://{}example.co.uk/x", "a.".repeat(100));
    let path = format!("https://evil.example/{}", "x".repeat(400));
    let unicode = format!("https://evil.example/{}", "猫".repeat(400));
    let unknown = format!("https://{}", "x".repeat(SCAN_BYTES + 1));
    for (name, url, cols) in [
        ("empty", "", 80),
        ("short", "https://evil.example/x", 80),
        ("unicode-host", "https://gооgle.com.猫.example/x", 80),
        ("hostile", "https://a\u{202e}b.example/x", 80),
        ("long-host", host.as_str(), 80),
        ("many-labels", labels.as_str(), 80),
        ("long-path", path.as_str(), 80),
        ("unicode-path", unicode.as_str(), 80),
        ("literal", "https://[2001:db8::ffff:192.0.2.1]:8443/x", 80),
        (
            "literal-narrow",
            "https://[2001:db8::ffff:192.0.2.1]:8443/x",
            50,
        ),
        ("unknown", unknown.as_str(), 80),
        ("zero", host.as_str(), 0),
        ("one", "https://猫.example/x", 1),
        ("blocked", "https://evil.example/a b", 80),
    ] {
        assert_eq!(caption(url, cols), previous_caption(url, cols));
        assert_eq!(
            caption_row(url, cols, Theme::default(), true, false),
            previous_row(url, cols, Theme::default(), true, false)
        );
        let mut formatters: [(&str, Formatter); 2] =
            [("reference", previous_caption), ("candidate", caption)];
        let mut rows: [(&str, Row); 2] = [("reference", previous_row), ("candidate", caption_row)];
        for sample in 0..5 {
            if sample != 0 {
                formatters.swap(0, 1);
                rows.swap(0, 1);
            }
            for (variant, probe) in formatters {
                for _ in 0..100 {
                    black_box(probe(url, cols));
                }
                let (_, allocations) = aterm_heap_count::allocations_in(|| probe(url, cols));
                let before = instructions();
                for _ in 0..2000 {
                    black_box(probe(black_box(url), black_box(cols)));
                }
                let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "url-format case={name} scope=formatter variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                );
            }
            for (variant, probe) in rows {
                let theme = Theme::default();
                for _ in 0..100 {
                    black_box(probe(url, cols, theme, true, false));
                }
                let (_, allocations) =
                    aterm_heap_count::allocations_in(|| probe(url, cols, theme, true, false));
                let before = instructions();
                for _ in 0..2000 {
                    black_box(probe(
                        black_box(url),
                        black_box(cols),
                        black_box(theme),
                        true,
                        false,
                    ));
                }
                let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "url-format case={name} scope=row variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                );
            }
            for _ in 0..100 {
                black_box(crate::is_safe_url(url));
            }
            let (_, allocations) = aterm_heap_count::allocations_in(|| crate::is_safe_url(url));
            let before = instructions();
            for _ in 0..2000 {
                black_box(crate::is_safe_url(black_box(url)));
            }
            let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
            println!(
                "url-format case={name} scope=safety variant=current sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
            );
        }
    }
}
