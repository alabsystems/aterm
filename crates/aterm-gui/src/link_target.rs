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
pub(crate) mod caption_test_support;

#[cfg(test)]
mod url_caption_tests;

#[cfg(test)]
mod literal_caption_tests;

#[cfg(test)]
mod encoding_tests;

#[cfg(test)]
mod row_words_tests;

#[cfg(test)]
#[path = "link_target_tests.rs"]
mod tests;
