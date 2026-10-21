// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The link target caption: a one-row chrome band that says where the hyperlink under the
//! pointer goes. OSC 8 lets the visible text and the destination be unrelated, so the underline
//! on a linked cell says only "this is a link". The band reuses the `paste_banner` mechanics (a
//! pure `RenderCell` row builder plus a splice that overwrites one frame row in place).
//!
//! A plain-text URL's visible text is its destination, so it gets the band only when some cell
//! of it is painted under 3:1 contrast and nobody can tell where it ends
//! (`crate::plain_url_at`), and only while the link modifier is held, since a plain URL is found
//! only then.
//!
//! Disclosure only: nothing here gates or changes what Cmd/ctrl-click opens; that boundary is
//! `is_safe_url` at the point of action.
//!
//! # The caption is derived, never remembered
//!
//! A hover keeps only [`LinkHover`], where the pointer is. The destination, and whether there
//! is a caption at all, are re-read by `App::splice_link_target` from the viewport it paints,
//! every frame: a remembered URL goes stale with no pointer event to announce it (the program
//! rewrites the cell, a tab moves to the front, a panel opens over the row).
//!
//! # Why the caption cannot itself deceive
//!
//! The string came from a hostile program, so no input may make it read as another
//! destination:
//!
//! * Every byte outside printable ASCII is percent-encoded ([`safe_form`]), so a bidi override,
//!   a control, a zero-width space, a soft hyphen or a Cyrillic homoglyph shows as visible
//!   `%XX`. The engine already rejects OSC 8 URIs with controls or bidi overrides
//!   (`handler_osc::handle_osc_8`, CVE-2021-42574); this holds for whatever a future admission
//!   rule lets through.
//! * Elision never drops the host. Keeping the head and appending an ellipsis is the forgery
//!   (`https://google.com.<400 chars>.evil.example/` reads as `google.com`), so the cut anchors
//!   on the end of the host ([`host_span`]) behind a leading ellipsis.
//! * The emphasized run is the site's own labels, the tail of the host ([`emphasis_span`]),
//!   not the whole authority, which is where the forgery puts its padding.
//! * Userinfo cannot pose as the host: the host follows the last `@` of the authority, found
//!   the way the browser finds it, over the whole URI ([`SCAN_BYTES`]). Where no host can be
//!   found the band shows the scheme and an ellipsis, never the head.
//!
//! # The seam has its own ink
//!
//! The band's content-facing edge is an overline painted from its own channel
//! (`RenderCell::overline_color`, resolved by `aterm_render::deco_inks`) rather than each
//! cell's `fg`, so it is one tone across the row. That lets the caption carry two inks: the
//! host takes the band's full-contrast tone, the gesture hint, scheme and path the secondary.

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

/// How many bytes of a URL are read: the engine's OSC 8 admission cap
/// (`MAX_HYPERLINK_URL_BYTES`), so every URI is read whole. No shorter bound is safe: the
/// userinfo before the host has no length limit. The read only searches for the ASCII
/// delimiters [`host_span`] needs; [`safe_form`] spells out only the characters painted.
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

/// Where the pointer rests, when the cell under it carried a destination to disclose at the
/// last hover resolution: an OSC 8 hyperlink, or a plain URL partly painted too faintly
/// (`crate::PlainUrl`). Not the URL: that is re-derived from the live grid at paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LinkHover {
    /// The pane-local `(row, col)` the pointer is on, in the viewport rows the
    /// frame is drawn in: the painted cell. The probe reads the logical cell
    /// under it (`app_mouse::pointer_cell`: a doubled line's halving, then the
    /// BiDi permutation), the one `Terminal::hyperlink_at_visible` addresses.
    /// Re-read, not remembered as a URL.
    pub(crate) cell: (u16, u16),
    /// Window terminal row of that cell: the placement may not cover it, and the splice
    /// checks whether chrome has taken the pointer's row.
    pub(crate) window_row: u16,
    /// The session the cell belongs to. The front terminal changes under a stationary pointer
    /// (tab switch, pane focus, migration), so the splice re-reads only while the front
    /// terminal is still this one.
    pub(crate) session: u64,
}

/// One URL laid out for a band `max_cells` wide: the text to paint and the host's run in it.
/// `host` counts characters, which here are also cells (printable ASCII or the one-cell
/// ellipsis), not bytes: the ellipsis is three bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caption {
    pub(crate) text: String,
    /// The whole host, in characters: what the elision anchors on.
    pub(crate) host: Range<usize>,
}

impl Caption {
    /// The emphasized run: the site's own labels within [`Self::host`] ([`emphasis_span`]).
    pub(crate) fn emphasis(&self) -> Range<usize> {
        let chars: Vec<char> = self.text.chars().collect();
        emphasis_span(&chars, &self.host)
    }

    /// The text of a run, in characters (see [`Caption`]).
    pub(crate) fn slice(&self, run: &Range<usize>) -> String {
        self.text.chars().skip(run.start).take(run.len()).collect()
    }

    /// The whole host's text, which the elision tests measure; the paint path uses
    /// [`Self::emphasis`].
    #[cfg(test)]
    pub(crate) fn host_text(&self) -> String {
        self.slice(&self.host)
    }

    /// The emphasized run's text.
    #[cfg(test)]
    pub(crate) fn emphasis_text(&self) -> String {
        self.slice(&self.emphasis())
    }
}

/// The run of the host a hurried reader should land on: the site's own labels, never the
/// padding in front of them. The last two labels, or three when the second-to-last has three
/// characters or fewer (`co.uk`, `com.au`). Erring long is safe: one label too many still ends
/// at the addressed site.
///
/// A reading aid, not a boundary: there is no public-suffix list here, and the security
/// argument rests on the whole host being shown and elided last ([`caption`]). An address
/// literal (`1.2.3.4`, `[::1]:8443`) is the run whole.
fn emphasis_span(chars: &[char], host: &Range<usize>) -> Range<usize> {
    let Some(slice) = chars.get(host.start..host.end) else {
        return host.clone();
    };
    if slice
        .iter()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ':' | '[' | ']'))
    {
        return host.clone();
    }
    let mut dots: Vec<usize> = slice
        .iter()
        .enumerate()
        .filter(|(_, ch)| **ch == '.')
        .map(|(i, _)| i)
        .collect();
    // A fully qualified host ends in the root's empty label (`evil.example.`). Counting that
    // dot would shift the run one label and leave `evil.` at padding weight, so it is dropped
    // from the count; it stays inside the run, which still reaches `host.end`.
    if dots.last() == Some(&slice.len().saturating_sub(1)) {
        dots.pop();
    }
    let Some(&last) = dots.last() else {
        return host.clone(); // one label: the whole thing names the site
    };
    let n = dots.len();
    if n < 2 {
        return host.clone();
    }
    let second_last_len = last - dots[n - 2] - 1;
    let keep = if second_last_len <= 3 { 3 } else { 2 };
    let start = if n >= keep { dots[n - keep] + 1 } else { 0 };
    host.start + start..host.end
}

/// Re-spell `text` so that nothing in it can reorder, hide inside, or
/// impersonate the rest of the line: printable ASCII survives verbatim, every
/// other character becomes the percent-encoding of its UTF-8 bytes.
///
/// Space is encoded too: `is_safe_url` refuses a raw space, and `%20` is its URL spelling.
fn safe_form(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if safe_width(ch) == 1 {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(HEX[usize::from(b >> 4)]);
                out.push(HEX[usize::from(b & 0x0f)]);
            }
        }
    }
    out
}

/// How many characters `ch` takes in the [`safe_form`]: one for printable
/// ASCII, three (`%XX`) for each UTF-8 byte of anything else.
fn safe_width(ch: char) -> usize {
    if matches!(ch, '\u{21}'..='\u{7e}') {
        1
    } else {
        3 * ch.len_utf8()
    }
}

/// The byte range of the host inside a URL: the run the band refuses to elide. Every delimiter
/// is ASCII, which never occurs inside a multi-byte UTF-8 sequence, so it reads the raw URL;
/// [`caption`] converts the result to safe-form characters.
///
/// For `scheme://` it is the authority minus any `userinfo@`; for a schemeless-authority form
/// such as `mailto:` it is the recipient, up to the first `?` or `#`.
///
/// A WHATWG special scheme is read the way the browser that opens it reads it: any run of `/`
/// and `\` after the colon is skipped (`https:///`, `https:\\` and a bare `https:` all start
/// the authority there), and `\` ends the authority like `/`. `file` is special too, but it has
/// its own host grammar and no click opens it, so it keeps the generic reading.
fn host_span(url: &str) -> Range<usize> {
    let Some(colon) = url.find(':') else {
        return 0..url.len(); // no scheme: the whole thing is the identity
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

/// The one-cell mark that says text was dropped.
const ELLIPSIS: char = '\u{2026}';

/// Lay `url` out for a band `max_cells` wide. When the head does not reach the end of the
/// host, the window anchors on the host's end behind a leading ellipsis (and a trailing one
/// when something follows the host), so the site's labels are the last thing to go. A URL
/// with no host to anchor on shows its scheme alone.
pub(crate) fn caption(url: &str, max_cells: usize) -> Caption {
    let raw = url.trim();
    let mut read = raw.len().min(SCAN_BYTES);
    while !raw.is_char_boundary(read) {
        read -= 1;
    }
    let span = host_span(&raw[..read]);
    // A host is known when it has a character and its authority ended inside the read; one
    // still running where the read stopped names nothing yet.
    let known = !span.is_empty() && (span.end < read || read == raw.len());
    // From here on, offsets are in the safe form's characters (and cells).
    let width = |text: &str| text.chars().map(safe_width).sum::<usize>();
    let host_start = width(&raw[..span.start]);
    let host = host_start..host_start + width(&raw[span.clone()]);
    // The head, spelled out only as far as the band could ever show it.
    let head_end = raw
        .char_indices()
        .nth(max_cells.saturating_add(1))
        .map_or(raw.len(), |(at, _)| at);
    let head = safe_form(&raw[..head_end]);
    if head_end == raw.len() && head.len() <= max_cells {
        return Caption { text: head, host };
    }
    match max_cells {
        0 => Caption {
            text: String::new(),
            host: 0..0,
        },
        1 => Caption {
            text: ELLIPSIS.to_string(),
            host: 0..0,
        },
        max if !known => {
            // No host to anchor on: nothing is emphasized and nothing past the scheme shows,
            // since it would stand where the host belongs. A scheme with a dot reads as a
            // domain, so only a plain one is named.
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
            Caption { text, host: 0..0 }
        }
        max if host.end < max => {
            // The host ends inside the head, so the ordinary cut is honest. The
            // head is longer than the band (that is why it is cut) and ASCII.
            let mut text = head[..max - 1].to_string();
            text.push(ELLIPSIS);
            Caption { text, host }
        }
        max => {
            // Anchor on the host's end: `…<tail of the host>…`, the trailing mark only when
            // something really follows the host.
            let after_host = span.end < raw.len();
            let keep = if after_host { max - 2 } else { max - 1 };
            // The last `keep` safe characters come from at most that many raw
            // ones, so only those are spelled out.
            let from = match keep.checked_sub(1) {
                Some(back) => raw[..span.end]
                    .char_indices()
                    .rev()
                    .nth(back)
                    .map_or(0, |(at, _)| at),
                None => span.end,
            };
            let tail = safe_form(&raw[from..span.end]);
            let start = host.end - keep;
            let mut text = String::with_capacity(max);
            text.push(ELLIPSIS);
            text.push_str(&tail[tail.len() - keep..]);
            if after_host {
                text.push(ELLIPSIS);
            }
            let host = 1 + host.start.max(start) - start..1 + keep;
            Caption { text, host }
        }
    }
}

/// Pure row builder: exactly `cols` cells, so the splice overwrites one frame row in place.
/// The lead-in names the gesture, the site's own labels are the one bold, full-contrast run,
/// and everything else takes the secondary tone.
///
/// `seam` draws the content-facing top edge (a bottom-anchored caption wants it; one flipped
/// to the top row is already under the strip's rule). `tracking`: the program under the
/// pointer tracks the mouse, so the lead-in names the gesture that still opens the link.
///
/// `None` when the band leaves the URL fewer than [`MIN_URL_CELLS`] or the URL contributes no
/// character: a row of output is too expensive for a band that names no site.
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
    // Re-stamp the emphasized run over the text just written: one layout, so the emphasis
    // cannot land on a different span than its text.
    let emphasis = cap.emphasis();
    if !emphasis.is_empty() {
        write_str(
            &mut row,
            cols,
            url_col + emphasis.start,
            &cap.slice(&emphasis),
            c.value,
            c.bar_bg,
            true,
        );
    }
    // The seam closes across the finished row in the band's ink (see the module doc).
    if seam {
        chrome_band::seal_band_top(&mut row, c.seam);
    }
    Some(row)
}

/// The lead-in of a file citation's caption (`crate::path_link`): the click
/// opens aterm's own editor, never the OS, and the band says so.
pub(crate) const OPENS_IN_ATERM: &str = "opens in aterm:";

/// The same lead-in while the program under the pointer tracks the mouse:
/// the gesture that still reaches aterm.
#[cfg(target_os = "macos")]
const OPENS_IN_ATERM_TRACKED: &str = "Cmd-Option-click opens in aterm:";
#[cfg(not(target_os = "macos"))]
const OPENS_IN_ATERM_TRACKED: &str = "ctrl+alt+click opens in aterm:";

/// A file citation's name laid out for `max_cells`: its safe form
/// ([`safe_form`]), cut from the FRONT when too wide so the file name and its
/// line, the answer, are the last thing to go. Returns the text and the run
/// to emphasize (the final path component with its `:line`), in characters.
pub(crate) fn path_caption(shown: &str, max_cells: usize) -> (String, Range<usize>) {
    let safe = safe_form(shown);
    let chars: Vec<char> = safe.chars().collect();
    let text: Vec<char> = if chars.len() <= max_cells {
        chars
    } else if max_cells == 0 {
        Vec::new()
    } else {
        let keep = max_cells - 1;
        std::iter::once(ELLIPSIS)
            .chain(chars[chars.len() - keep..].iter().copied())
            .collect()
    };
    let name_start = text
        .iter()
        .rposition(|ch| *ch == '/')
        .map_or(0, |at| at + 1)
        .max(usize::from(text.first() == Some(&ELLIPSIS)));
    (text.iter().collect(), name_start..text.len())
}

/// PURE row builder for a file citation (`crate::path_link`):
/// `opens in aterm: <path>:<line>`, the file name and line emphasized, the
/// same band as [`caption_row`] (`cols` wide, the seam when asked). `None`
/// when the band leaves fewer than [`MIN_URL_CELLS`] for the name.
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
        let run: String = text
            .chars()
            .skip(emphasis.start)
            .take(emphasis.len())
            .collect();
        write_str(
            &mut row,
            cols,
            name_col + emphasis.start,
            &run,
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
    /// authority), not RFC 3986's. Expected hosts are node's `new URL(u).host`.
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
    /// Expected hosts are node's `new URL(u).host`.
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
    /// show. Every width from nothing to wider than the URL.
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
        // The same URL with a path keeps the mark.
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
        // Narrow: the hint goes, the destination stays.
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
        // Literal widths: bounds derived from `MIN_URL_CELLS` would follow the constant and
        // assert nothing.
        for cols in 0..=11usize {
            assert!(
                caption_row("https://evil.example/steal", cols, theme, true, false).is_none(),
                "cols={cols} leaves room for `\u{2026}ple\u{2026}` at best, which names \
                 no site, and must not take a row to paint it"
            );
        }
        // Twelve is the first width that shows a recognizable run of the host.
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
        // An empty URL contributes no character, so it declines the row at any width.
        assert!(caption_row("", 200, theme, true, false).is_none());
    }

    /// The destination is the brightest thing on the row, on every theme the
    /// product ships. Weight is a hierarchy a glance can miss — a bold run in a
    /// dim tone still reads as part of the dim sentence around it — and the one
    /// question this band answers is which site. So the site's own labels take
    /// the band's full-contrast ink and everything else stays secondary, while
    /// the seam underneath is unaffected either way.
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
            // Fully qualified: the root's empty label must not shift the run.
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
