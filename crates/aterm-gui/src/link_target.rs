// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The link target caption.

use std::ops::Range;

use aterm_core::terminal::RenderCell;
#[cfg(test)]
use aterm_render::Theme;

use crate::chrome_band;
use crate::settings::{blank_row, write_str};

/// The caption's left/right margin, in cells (mirrors `paste_banner`).
const MARGIN: usize = 2;
/// Gap between the lead-in and the URL.
const GAP: usize = 2;
/// Below this many cells for the URL itself the lead-in is dropped: the
/// destination is the disclosure, the gesture hint is the courtesy.
const LEAD_IN_URL_CELLS: usize = 12;
/// Below this many cells for the URL the band paints nothing: an elided `…<text>…` that narrow
/// is a fragment of a hostname that names no site, not worth a row of output.
const MIN_URL_CELLS: usize = 8;

/// The painted caption's words for a client, without allocating its padding.
pub(crate) fn row_words(row: &[RenderCell]) -> String {
    let Some(start) = row.iter().position(|cell| !cell.ch.is_whitespace()) else {
        return String::new();
    };
    let end = row
        .iter()
        .rposition(|cell| !cell.ch.is_whitespace())
        .unwrap()
        + 1;
    let kept = &row[start..end];
    let mut words = String::with_capacity(kept.iter().map(|cell| cell.ch.len_utf8()).sum());
    words.extend(kept.iter().map(|cell| cell.ch));
    words
}

/// How many bytes host discovery examines: the engine's OSC 8 admission cap
/// (`MAX_HYPERLINK_URL_BYTES`), so every admitted URI's host is read whole.
const SCAN_BYTES: usize = 8 * 1024;

const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];

/// The gesture that opens a link: Cmd on macOS, Ctrl elsewhere, the split
/// `app_mouse::link_modifier_held` makes.
#[cfg(target_os = "macos")]
const OPENS: &str = "Cmd-click opens";
#[cfg(not(target_os = "macos"))]
const OPENS: &str = "ctrl+click opens";

/// The gesture while the program under the pointer tracks the mouse: a plain
/// press goes to the program, and Option/Alt takes it back
/// (`app_mouse::press_starts_selection`).
#[cfg(target_os = "macos")]
const OPENS_TRACKED: &str = "Cmd-Option-click opens";
#[cfg(not(target_os = "macos"))]
const OPENS_TRACKED: &str = "ctrl+alt+click opens";

/// The lead-in for a link whose scheme `crate::is_safe_url` refuses: the click is a no-op, so
/// it must not say "opens".
const BLOCKED: &str = "blocked scheme";

/// Where the pointer rests, when the cell under it carried a destination to disclose at the last hover resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LinkHover {
    /// The pane-local `(row, col)` the pointer is on, in the viewport rows the frame is drawn in.
    pub(crate) cell: (u16, u16),
    /// Window terminal row of that cell.
    pub(crate) window_row: u16,
    /// The session the cell belongs to.
    pub(crate) session: u64,
}

/// One URL laid out for a band `max_cells` wide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caption {
    pub(crate) text: String,
    /// The visible host run, in characters.
    pub(crate) host: Range<usize>,
    /// Original host shape, retained when elision drops its opening bracket.
    shape: HostShape,
}

/// A bracketed authority is one identity; its dots never separate DNS labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostShape {
    Labels,
    Bracketed,
}

impl Caption {
    /// The emphasized run.
    pub(crate) fn emphasis(&self) -> Range<usize> {
        match self.shape {
            HostShape::Labels => emphasis_span(&self.text, &self.host),
            HostShape::Bracketed => self.host.clone(),
        }
    }

    /// The text of a run, in characters (see [`Caption`]).
    pub(crate) fn slice(&self, run: &Range<usize>) -> &str {
        char_slice(&self.text, run)
    }

    /// The whole host's text, which the elision tests measure.
    #[cfg(test)]
    pub(crate) fn host_text(&self) -> &str {
        self.slice(&self.host)
    }

    /// The emphasized run's text.
    #[cfg(test)]
    pub(crate) fn emphasis_text(&self) -> &str {
        self.slice(&self.emphasis())
    }
}

/// Borrow a character-indexed run, clamping it as `chars().skip().take()` does.
fn char_slice<'a>(text: &'a str, run: &Range<usize>) -> &'a str {
    if run.is_empty() {
        return "";
    }
    if text.is_ascii() {
        let from = run.start.min(text.len());
        let kept = run.len().min(text.len() - from);
        return &text[from..from + kept];
    }
    let from = text
        .char_indices()
        .nth(run.start)
        .map_or(text.len(), |(at, _)| at);
    let to = text[from..]
        .char_indices()
        .nth(run.len())
        .map_or(text.len(), |(at, _)| from + at);
    &text[from..to]
}

/// The run of the host a hurried reader should land on: the site's own labels, never the
/// padding in front of them.
fn emphasis_span(text: &str, host: &Range<usize>) -> Range<usize> {
    if host.is_empty() {
        return host.clone();
    }
    let mut dots = [0; 3];
    let mut n = 0;
    let mut width = 0;
    let mut numeric = true;
    for (at, ch) in char_slice(text, host).chars().enumerate() {
        width = at + 1;
        numeric &= ch.is_ascii_digit() || matches!(ch, '.' | ':' | '[' | ']');
        // Exclude the one trailing dot. Only the last three other separators
        // can affect this rule, regardless of how many padding labels precede them.
        if ch == '.' && width < host.len() {
            dots = [dots[1], dots[2], at];
            n += 1;
        }
    }
    if width != host.len() || numeric || n < 2 {
        return host.clone();
    }
    let second_last_len = dots[2] - dots[1] - 1;
    let keep = if second_last_len <= 3 { 3 } else { 2 };
    let start = if n >= keep { dots[3 - keep] + 1 } else { 0 };
    host.start + start..host.end
}

/// Characters in one encoded byte: printable ASCII survives; everything else is %XX.
const fn encoded_byte_width(byte: u8) -> usize {
    if byte.is_ascii_graphic() { 1 } else { 3 }
}

/// Encoded character count, without decoding and re-encoding UTF-8 characters.
fn encoded_width(bytes: &[u8]) -> usize {
    bytes.len()
        + 2 * bytes
            .iter()
            .filter(|&&byte| !byte.is_ascii_graphic())
            .count()
}

/// Append complete encoded bytes. The caller reserves the output's exact byte length.
#[inline]
fn append_safe(out: &mut String, bytes: &[u8]) {
    for &byte in bytes {
        if byte.is_ascii_graphic() {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)]);
            out.push(HEX[usize::from(byte & 0x0f)]);
        }
    }
}

/// Append a selected tail, omitting `skip` encoded characters at its beginning.
/// A cut may fall inside a UTF-8 character or an escape; input is deliberately bytes.
fn append_safe_tail(out: &mut String, mut bytes: &[u8], mut skip: usize) {
    while skip != 0 {
        let Some((&first, rest)) = bytes.split_first() else {
            return;
        };
        let width = encoded_byte_width(first);
        if skip >= width {
            skip -= width;
            bytes = rest;
            continue;
        }
        // A positive remainder can only straddle a three-character escape.
        let escape = [
            '%',
            HEX[usize::from(first >> 4)],
            HEX[usize::from(first & 0x0f)],
        ];
        for &ch in escape.iter().skip(skip) {
            out.push(ch);
        }
        bytes = rest;
        break;
    }
    append_safe(out, bytes);
}

/// Append at most `keep` encoded characters, without encoding the discarded suffix.
fn append_safe_prefix(out: &mut String, bytes: &[u8], mut keep: usize) {
    if keep == 0 {
        return;
    }
    for (at, &byte) in bytes.iter().enumerate() {
        let width = encoded_byte_width(byte);
        if width > keep {
            append_safe(out, &bytes[..at]);
            let escape = [
                '%',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 0x0f)],
            ];
            for &ch in escape.iter().take(keep) {
                out.push(ch);
            }
            return;
        }
        keep -= width;
        if keep == 0 {
            append_safe(out, &bytes[..at + 1]);
            return;
        }
    }
    append_safe(out, bytes);
}

/// The byte range of the host inside a URL. Every delimiter
/// is ASCII, which never occurs inside a multi-byte UTF-8 sequence, so it reads the raw URL;
/// [`caption`] maps the result to percent-encoded character positions.
fn host_span(url: &str) -> Range<usize> {
    let Some(colon) = url.find(':') else {
        // no scheme: the whole thing is the identity
        return 0..url.len();
    };
    let after = &url[colon + 1..];
    let special = ["http", "https", "ws", "wss", "ftp"]
        .iter()
        .any(|s| url[..colon].eq_ignore_ascii_case(s));
    let authority = if special {
        Some(after.trim_start_matches(['/', '\\']))
    } else {
        after.strip_prefix("//")
    };
    if let Some(authority) = authority {
        let start = url.len() - authority.len();
        let end = start
            + authority
                .bytes()
                .position(|byte| matches!(byte, b'/' | b'?' | b'#') || (special && byte == b'\\'))
                .unwrap_or(authority.len());
        let host_start = url[start..end]
            .rfind('@')
            .map_or(start, |at| start + at + 1);
        host_start..end
    } else {
        let end = colon
            + 1
            + after
                .bytes()
                .position(|byte| matches!(byte, b'?' | b'#'))
                .unwrap_or(after.len());
        colon + 1..end
    }
}

/// The one-cell mark that says text was dropped.
const ELLIPSIS: char = '\u{2026}';

/// Lay `url` out for a band `max_cells` wide.
pub(crate) fn caption(url: &str, max_cells: usize) -> Caption {
    let raw = url.trim();
    let mut read = raw.len().min(SCAN_BYTES);
    while !raw.is_char_boundary(read) {
        read -= 1;
    }
    let span = host_span(&raw[..read]);
    let shape = if raw[span.clone()].starts_with('[') {
        HostShape::Bracketed
    } else {
        HostShape::Labels
    };
    let finish = |text, host| Caption { text, host, shape };
    let known = !span.is_empty() && (span.end < read || read == raw.len());
    let host = || {
        let start = encoded_width(&raw.as_bytes()[..span.start]);
        start..start + encoded_width(&raw.as_bytes()[span.clone()])
    };
    // Escaping never shortens a byte string, so long inputs need no fit scan.
    if raw.len() <= max_cells {
        let width = encoded_width(raw.as_bytes());
        if width <= max_cells {
            let text = if width == raw.len() {
                // Equal lengths prove every byte is printable ASCII.
                raw.to_owned()
            } else {
                let mut text = String::with_capacity(width);
                append_safe(&mut text, raw.as_bytes());
                text
            };
            return finish(text, host());
        }
    }
    match max_cells {
        0 => finish(String::new(), 0..0),
        1 => finish(ELLIPSIS.to_string(), 0..0),
        max if !known => {
            let scheme = raw[..read]
                .split_once(':')
                .map(|(scheme, _)| scheme)
                .filter(|scheme| {
                    !scheme.is_empty()
                        && scheme.len() + 2 <= max
                        && scheme
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-'))
                });
            let prefix = scheme.map_or(0, |scheme| scheme.len() + 1);
            let mut text = String::with_capacity(prefix + ELLIPSIS.len_utf8());
            if let Some(scheme) = scheme {
                text.push_str(scheme);
                text.push(':');
            }
            text.push(ELLIPSIS);
            finish(text, 0..0)
        }
        max => {
            let host = host();
            if host.end < max {
                let keep = max - 1;
                let mut text = String::with_capacity(keep + ELLIPSIS.len_utf8());
                append_safe_prefix(&mut text, raw.as_bytes(), keep);
                text.push(ELLIPSIS);
                return finish(text, host);
            }
            let after_host = span.end < raw.len();
            let keep = if after_host { max - 2 } else { max - 1 };
            let mut from = span.end;
            let mut width = 0;
            while from > 0 && width < keep {
                from -= 1;
                width += encoded_byte_width(raw.as_bytes()[from]);
            }
            let start = host.end - keep;
            let ellipses = 1 + usize::from(after_host);
            let mut text = String::with_capacity(keep + ellipses * ELLIPSIS.len_utf8());
            text.push(ELLIPSIS);
            append_safe_tail(
                &mut text,
                &raw.as_bytes()[from..span.end],
                width.saturating_sub(keep),
            );
            if after_host {
                text.push(ELLIPSIS);
            }
            let host = 1 + host.start.max(start) - start..1 + keep;
            finish(text, host)
        }
    }
}

/// Pure row builder.
pub(crate) fn caption_row(
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
    let cap = caption(url, url_cells);
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

/// The lead-in of a file citation's caption (`crate::path_link`).
pub(crate) const OPENS_IN_ATERM: &str = "opens in aterm:";

/// The same lead-in while the program under the pointer tracks the mouse:
/// the gesture that still reaches aterm.
#[cfg(target_os = "macos")]
const OPENS_IN_ATERM_TRACKED: &str = "Cmd-Option-click opens in aterm:";
#[cfg(not(target_os = "macos"))]
const OPENS_IN_ATERM_TRACKED: &str = "ctrl+alt+click opens in aterm:";

/// A file citation's name laid out for `max_cells`.
pub(crate) fn path_caption(shown: &str, max_cells: usize) -> (String, Range<usize>) {
    if max_cells == 0 || shown.is_empty() {
        return (String::new(), 0..0);
    }
    // Each printable ASCII byte survives; every other UTF-8 byte becomes %XX.
    // Read only enough of the tail to decide whether it fits, including one
    // byte beyond the budget. Bytes let a clipped tail retain even part of an
    // escape, exactly as truncating the complete safe form does.
    let mut from = shown.len();
    let mut width = 0;
    for &byte in shown.as_bytes().iter().rev() {
        from -= 1;
        width += encoded_byte_width(byte);
        if width > max_cells {
            break;
        }
    }
    let clipped = width > max_cells;
    let kept = if clipped { max_cells - 1 } else { width };
    let mut text = String::with_capacity(kept + if clipped { ELLIPSIS.len_utf8() } else { 0 });
    if clipped {
        text.push(ELLIPSIS);
    }
    if !clipped && width == shown.len() {
        text.push_str(shown);
    } else if kept != 0 {
        append_safe_tail(&mut text, &shown.as_bytes()[from..], width - kept);
    }
    let name_start = text.rfind('/').map_or(usize::from(clipped), |at| {
        // All output after the optional leading ellipsis is ASCII.
        at + 1 - usize::from(clipped) * (ELLIPSIS.len_utf8() - 1)
    });
    (text, name_start..kept + usize::from(clipped))
}

/// PURE row builder for a file citation (`crate::path_link`).
pub(crate) fn path_caption_row(
    shown: &str,
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
    let lead = if tracking {
        OPENS_IN_ATERM_TRACKED
    } else {
        OPENS_IN_ATERM
    };
    let gap = 1;
    let with_lead = body.saturating_sub(lead.len() + gap);
    let (name_col, name_cells) = if with_lead >= LEAD_IN_URL_CELLS {
        write_str(&mut row, cols, MARGIN, lead, c.label, c.bar_bg, false);
        (MARGIN + lead.len() + gap, with_lead)
    } else {
        (MARGIN, body)
    };
    let (text, emphasis) = path_caption(shown, name_cells);
    if !text.chars().any(|ch| ch != ELLIPSIS) {
        return None;
    }
    write_str(&mut row, cols, name_col, &text, c.label, c.bar_bg, false);
    if !emphasis.is_empty() {
        // The emphasized suffix is safe-form ASCII; a leading elision is excluded.
        let run = &text[text.len() - emphasis.len()..];
        write_str(
            &mut row,
            cols,
            name_col + emphasis.start,
            run,
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

#[cfg(test)]
mod path_caption_tests;

#[cfg(test)]
mod caption_test_support;

#[cfg(test)]
mod url_caption_tests;

#[cfg(test)]
mod literal_caption_tests;

#[cfg(test)]
mod encoding_tests;

#[cfg(test)]
mod row_words_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    /// Trojan Source at the display end: a right-to-left override reaches the band as `%XX`.
    #[test]
    fn a_bidi_override_is_spelled_out_instead_of_reordering_the_caption() {
        let cap = caption("https://safe.example\u{202e}moc.live/x", 200);
        assert!(
            !cap.text.chars().any(|c| c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
            "{:?}",
            cap.text
        );
        assert!(cap.text.contains("%E2%80%AE"), "{:?}", cap.text);
    }

    /// Controls, a zero-width space, a soft hyphen, a BOM, a Cyrillic homoglyph and a space each
    /// leave as `%XX`.
    #[test]
    fn invisible_and_look_alike_characters_survive_only_as_percent_escapes() {
        for hostile in [
            "https://a\u{0007}b.example/",
            "https://a\u{200b}b.example/",
            "https://a\u{00ad}b.example/",
            "https://\u{feff}b.example/",
            "https://g\u{043e}\u{043e}gle.com/",
            "https://a b.example/",
        ] {
            let cap = caption(hostile, 200);
            assert!(
                cap.text.chars().all(|c| matches!(c, '\u{21}'..='\u{7e}')),
                "{hostile:?} -> {:?}",
                cap.text
            );
            assert!(cap.text.contains('%'), "{hostile:?} -> {:?}", cap.text);
        }
    }

    /// The long-URL forgery: the cut keeps the end of the host, never `https://google.com…`.
    #[test]
    fn eliding_a_long_url_keeps_the_host_and_never_the_forged_head() {
        let url = format!("https://google.com.{}.evil.example/steal", "a".repeat(400));
        let cap = caption(&url, 40);
        assert_eq!(cap.text.chars().count(), 40, "{:?}", cap.text);
        assert!(cap.text.contains("evil.example"), "{:?}", cap.text);
        assert!(
            !cap.text.starts_with("https://google.com"),
            "the head must not be the surviving half: {:?}",
            cap.text
        );
        assert!(cap.text.starts_with(ELLIPSIS), "{:?}", cap.text);
        assert!(cap.text.ends_with(ELLIPSIS), "{:?}", cap.text);
        assert!(
            cap.host_text().ends_with("evil.example"),
            "the kept run IS the host, and it is the emphasized one: {:?}",
            cap.host_text()
        );
    }

    /// Everything before the last `@` is userinfo: the host is `evil.example`.
    #[test]
    fn userinfo_before_an_at_sign_is_never_emphasized_as_the_host() {
        let cap = caption("https://google.com@evil.example/steal", 200);
        assert_eq!(cap.text, "https://google.com@evil.example/steal");
        assert_eq!(cap.host_text(), "evil.example");
    }

    /// A special scheme's host is the WHATWG parser's (runs of `/` and `\` skipped, `\` ends the
    /// authority), not RFC 3986's.
    #[test]
    fn a_special_schemes_host_is_the_one_the_browser_parses() {
        for (url, want) in [
            ("https:///evil.example/steal", "evil.example"),
            ("https:\\\\evil.example/steal", "evil.example"),
            ("HTTPS:/\\/evil.example", "evil.example"),
            ("http:evil.example/x.google.com", "evil.example"),
            ("https://evil.example\\.google.com/", "evil.example"),
            ("https://evil.example\\@google.com/", "evil.example"),
            ("https:///google.com@evil.example/", "evil.example"),
        ] {
            assert_eq!(caption(url, 200).host_text(), want, "{url}");
        }
        let pad = ".a".repeat(100);
        for url in [
            format!("https:///google.com{pad}.evil.example/"),
            format!("https://evil.example\\{pad}.google.com/"),
        ] {
            let cap = caption(&url, 59);
            assert_eq!(cap.emphasis_text(), "evil.example", "{:?}", cap.text);
            assert!(!cap.text.starts_with("https:///google"), "{:?}", cap.text);
        }
    }

    /// A URL that fits is shown whole, and the host is still the emphasized run
    /// (port included — a port is part of which endpoint this reaches).
    #[test]
    fn a_url_that_fits_is_shown_whole_with_its_host_marked() {
        let cap = caption("https://evil.example:8443/steal?x=1", 200);
        assert_eq!(cap.text, "https://evil.example:8443/steal?x=1");
        assert_eq!(cap.host_text(), "evil.example:8443");
        let cap = caption("mailto:ops@evil.example?subject=hi", 200);
        assert_eq!(cap.host_text(), "ops@evil.example");
    }

    /// An ordinary over-long URL still cuts from the tail — the host fits, so
    /// the head is honest and the path is what goes.
    #[test]
    fn a_long_path_is_cut_from_the_tail_because_the_host_still_fits() {
        let url = format!("https://evil.example/{}", "p".repeat(400));
        let cap = caption(&url, 40);
        assert_eq!(cap.text.chars().count(), 40);
        assert!(
            cap.text.starts_with("https://evil.example/"),
            "{:?}",
            cap.text
        );
        assert!(cap.text.ends_with(ELLIPSIS));
        assert_eq!(cap.host_text(), "evil.example");
    }

    /// The whole URI is read: a host pushed far in by userinfo or a run of slashes is still found,
    /// and one whose authority outruns the whole read names no host, so only the scheme shows.
    #[test]
    fn the_host_is_found_however_far_into_the_uri_it_is_pushed() {
        for url in [
            format!("https://google.com{}@evil.example/", ".a".repeat(600)),
            format!("https:{}evil.example/", "/".repeat(1100)),
        ] {
            for max in [59, 200] {
                let cap = caption(&url, max);
                assert_eq!(cap.host_text(), "evil.example", "@{max}: {:?}", cap.text);
                assert_eq!(cap.emphasis_text(), "evil.example", "@{max}");
                assert!(cap.text.starts_with(ELLIPSIS), "@{max}: {:?}", cap.text);
            }
        }
        for url in [
            format!("https://{}", "a".repeat(SCAN_BYTES * 2)),
            format!(
                "https://google.com{}@evil.example/",
                ".a".repeat(SCAN_BYTES)
            ),
        ] {
            let cap = caption(&url, 60);
            assert_eq!(cap.text, "https:\u{2026}", "{:?}", cap.text);
            assert!(cap.host.is_empty(), "{:?}", cap.host);
        }
    }

    /// Degenerate widths must not panic and must never claim more than they
    /// show.
    #[test]
    fn degenerate_widths_never_panic_and_never_overrun_the_band() {
        let urls = [
            "https://evil.example/steal".to_string(),
            format!("https://google.com.{}.evil.example/", "a".repeat(300)),
            "mailto:a@b.c".to_string(),
            "https://".to_string(),
            "nonsense".to_string(),
            String::new(),
        ];
        for url in &urls {
            for max in 0..80usize {
                let cap = caption(url, max);
                let cells = cap.text.chars().count();
                assert!(cells <= max, "{url:?} @{max} -> {cells}");
                assert!(cap.host.start <= cap.host.end, "{url:?} @{max}");
                assert!(cap.host.end <= cells, "{url:?} @{max}");
            }
        }
    }

    /// A URL that ends at its host gets no trailing ellipsis; the same URL with a path does.
    #[test]
    fn a_url_that_ends_at_its_host_is_not_marked_as_having_more() {
        let url = format!("https://google.com.{}.evil.example", "a".repeat(400));
        let cap = caption(&url, 40);
        assert_eq!(cap.text.chars().count(), 40, "{:?}", cap.text);
        assert!(cap.text.starts_with(ELLIPSIS), "{:?}", cap.text);
        assert!(
            !cap.text.ends_with(ELLIPSIS),
            "nothing follows the host, so nothing was dropped after it: {:?}",
            cap.text
        );
        assert!(cap.text.ends_with("evil.example"), "{:?}", cap.text);
        assert!(cap.host_text().ends_with("evil.example"), "{:?}", cap.host);
        let cap = caption(&format!("{url}/steal"), 40);
        assert!(cap.text.ends_with(ELLIPSIS), "{:?}", cap.text);
    }

    /// The painted row is exactly `cols` wide at every width it is painted at,
    /// the destination outlives the gesture hint when the band gets narrow, and
    /// the host is the bold run.
    #[test]
    fn the_painted_row_is_exactly_cols_wide_and_bolds_only_the_host() {
        let theme = Theme::default();
        for cols in [20usize, 40, 80, 200] {
            let row = caption_row("https://evil.example/steal", cols, theme, true, false)
                .expect("a wide band");
            assert_eq!(row.len(), cols, "cols={cols}");
        }
        let row =
            caption_row("https://evil.example/steal", 80, theme, true, false).expect("a wide band");
        let text = text_of(&row);
        assert!(text.contains("https://evil.example/steal"), "{text:?}");
        assert!(text.contains(OPENS), "{text:?}");
        let bold: String = row
            .iter()
            .filter(|cell| cell.bold)
            .map(|cell| cell.ch)
            .collect();
        assert_eq!(bold, "evil.example");
        let row = caption_row("https://evil.example/steal", 26, theme, true, false)
            .expect("a narrow band");
        let text = text_of(&row);
        assert!(!text.contains(OPENS), "{text:?}");
        assert!(text.contains("evil.example"), "{text:?}");
    }

    /// A band too narrow to name a site takes no row.
    #[test]
    fn a_band_too_narrow_to_name_anything_does_not_take_a_row() {
        let theme = Theme::default();
        for cols in 0..=11usize {
            assert!(
                caption_row("https://evil.example/steal", cols, theme, true, false).is_none(),
                "cols={cols} leaves room for `\u{2026}ple\u{2026}` at best, which names \
                 no site, and must not take a row to paint it"
            );
        }
        let row = caption_row("https://evil.example/steal", 12, theme, true, false)
            .expect("12 cells suffice");
        let seen: String = text_of(&row).chars().filter(|c| *c != ' ').collect();
        assert_eq!(seen, "…xample…", "{seen:?}");
        let row = caption_row("https://evil.example/steal", 16, theme, true, false)
            .expect("16 cells carry a run of the host");
        let text = text_of(&row);
        assert!(
            text.chars().any(|ch| ch.is_ascii_alphanumeric()),
            "{text:?}"
        );
        assert!(caption_row("", 200, theme, true, false).is_none());
    }

    /// The destination is the brightest thing on the row, on every theme the
    /// product ships.
    #[test]
    fn the_sites_own_labels_are_the_only_run_at_full_contrast() {
        for name in aterm_types::scheme::builtin_names() {
            let theme =
                Theme::from(&aterm_types::scheme::builtin(name).expect("listed scheme exists"));
            let c = chrome_band::band_colors(theme);
            assert_ne!(
                c.value, c.label,
                "{name}: two tones or there is no hierarchy to assert"
            );
            let row = caption_row(
                "https://google.com@evil.example/steal",
                80,
                theme,
                true,
                false,
            )
            .expect("a wide band");
            let bright: String = row
                .iter()
                .filter(|cell| cell.fg == c.value)
                .map(|cell| cell.ch)
                .collect();
            assert_eq!(
                bright, "evil.example",
                "{name}: the full-contrast run must be the site and nothing else"
            );
            assert!(
                row.iter()
                    .all(|cell| cell.fg == c.value || cell.fg == c.label),
                "{name}: the band speaks in exactly two tones"
            );
            assert!(
                row.iter().all(|cell| cell.overline_color == Some(c.label)),
                "{name}: the seam keeps the secondary tone under the bright run"
            );
        }
    }

    /// With the forgery's padding inside the authority, only the site's own labels carry weight.
    #[test]
    fn the_forged_authoritys_padding_is_never_part_of_the_emphasized_run() {
        let url = format!("https://google.com.{}.evil.example/steal", "a".repeat(400));
        let cap = caption(&url, 60);
        assert!(
            cap.host_text().contains("aaaa"),
            "the padding really is inside the authority: {:?}",
            cap.host_text()
        );
        assert_eq!(cap.emphasis_text(), "evil.example");
        let row = caption_row(&url, 64, Theme::default(), true, false).expect("a wide band");
        let bold: String = row
            .iter()
            .filter(|cell| cell.bold)
            .map(|cell| cell.ch)
            .collect();
        assert_eq!(
            bold,
            "evil.example",
            "only the site's labels carry weight: {:?}",
            text_of(&row)
        );
    }

    /// The site's labels: two, or three where two name a registry (`co.uk`); an address
    /// literal whole; never userinfo.
    #[test]
    fn the_emphasized_run_is_the_sites_own_labels_and_never_a_bare_suffix() {
        for (url, want) in [
            ("https://evil.example/steal", "evil.example"),
            ("https://a.b.c.evil.example/", "evil.example"),
            ("https://google.com@evil.example/", "evil.example"),
            ("https://evil.co.uk/steal", "evil.co.uk"),
            ("https://pay.pal.com.evil.co.uk/", "evil.co.uk"),
            ("https://evil.example:8443/steal", "evil.example:8443"),
            ("https://10.0.0.7:8443/steal", "10.0.0.7:8443"),
            ("https://[2001:db8::1]/steal", "[2001:db8::1]"),
            ("https://localhost:9000/steal", "localhost:9000"),
            ("https://evil.example./steal", "evil.example."),
            (
                "https://google.com.pad.evil.example./steal",
                "evil.example.",
            ),
            ("https://evil.co.uk./steal", "evil.co.uk."),
            ("https://example./steal", "example."),
        ] {
            assert_eq!(caption(url, 200).emphasis_text(), want, "{url}");
        }
    }

    /// A scheme `is_safe_url` refuses is disclosed too — with a lead-in that
    /// does not promise a click will do anything.
    #[test]
    fn a_blocked_scheme_is_disclosed_without_promising_to_open_it() {
        let row = caption_row("file:///etc/passwd", 80, Theme::default(), true, false)
            .expect("a wide band");
        let text = text_of(&row);
        assert!(text.contains(BLOCKED), "{text:?}");
        assert!(!text.contains(OPENS), "{text:?}");
        assert!(text.contains("file:///etc/passwd"), "{text:?}");
    }

    /// While the program under the pointer tracks the mouse, a plain modifier
    /// press goes to the program (`app_mouse::press_starts_selection`), so the
    /// caption names the gesture that still opens the link.
    #[test]
    fn a_tracked_pane_names_the_gesture_that_still_opens_the_link() {
        let row = caption_row(
            "https://evil.example/steal",
            80,
            Theme::default(),
            true,
            true,
        )
        .expect("a wide band");
        let text = text_of(&row);
        assert!(text.contains(OPENS_TRACKED), "{text:?}");
        assert!(!text.contains(OPENS), "{text:?}");
        assert!(crate::app_mouse::press_starts_selection(true, true));
        assert!(!crate::app_mouse::press_starts_selection(true, false));
        let row = caption_row("file:///etc/passwd", 80, Theme::default(), true, true)
            .expect("a wide band");
        assert!(text_of(&row).contains(BLOCKED));
    }

    /// A file citation's band says where the click goes and that it stays in
    /// aterm: `opens in aterm: src/foo.rs:42`, the file name and line the
    /// emphasized run, and never the OS gesture's `opens`/`blocked` words.
    #[test]
    fn a_file_citation_caption_names_aterm_and_the_line() {
        let row =
            path_caption_row("src/foo.rs:42", 80, Theme::default(), true, false).expect("a band");
        let text = text_of(&row);
        assert!(text.contains("opens in aterm: src/foo.rs:42"), "{text:?}");
        assert!(!text.contains(BLOCKED), "{text:?}");
        let (shown, emphasis) = path_caption("src/foo.rs:42", 80);
        let run: String = shown
            .chars()
            .skip(emphasis.start)
            .take(emphasis.len())
            .collect();
        assert_eq!(run, "foo.rs:42");
        let bold: String = row
            .iter()
            .filter(|cell| cell.bold)
            .map(|cell| cell.ch)
            .collect();
        assert_eq!(bold, "foo.rs:42", "the file and its line are the answer");
    }

    /// Too wide for the band, the cut keeps the END: the file name and line.
    #[test]
    fn a_long_citation_is_cut_from_the_front() {
        let long = format!("{}/deep/file.rs:7", "a".repeat(300));
        let (text, emphasis) = path_caption(&long, 30);
        assert_eq!(text.chars().count(), 30, "{text:?}");
        assert!(
            text.starts_with(ELLIPSIS) && text.ends_with("file.rs:7"),
            "{text:?}"
        );
        let run: String = text
            .chars()
            .skip(emphasis.start)
            .take(emphasis.len())
            .collect();
        assert_eq!(run, "file.rs:7");
    }

    /// A hostile name (a bidi override in a file name) arrives as `%XX`, as
    /// a URL's does: the band cannot be made to reorder.
    #[test]
    fn a_hostile_file_name_is_spelled_out() {
        let (text, _) = path_caption("src/a\u{202e}sr.exe:1", 200);
        assert!(text.contains("%E2%80%AE"), "{text:?}");
        assert!(!text.contains('\u{202e}'));
        let row = path_caption_row("x.rs:1", 80, Theme::default(), true, true).expect("a band");
        assert!(text_of(&row).contains(OPENS_IN_ATERM_TRACKED));
    }
}
