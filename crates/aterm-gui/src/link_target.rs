// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LINK TARGET CAPTION: the one-row chrome band that says where the
//! hyperlink under the pointer actually goes.
//!
//! OSC 8 lets the visible text and the destination be unrelated — `google.com`
//! addressed to `evil.example` is one escape sequence — so the underline the
//! renderer stamps on a linked cell says "this is a link" and nothing at all
//! about WHERE. A mark that invites a click without disclosing its destination
//! is half an affordance; the other half is this band, which reuses the
//! `paste_banner` mechanics (a pure `RenderCell` row builder
//! plus a splice that overwrites one frame row in place) rather than inventing a
//! floating widget of its own.
//!
//! A plain-text URL needs no band: its visible text IS its destination. Except
//! where part of it is painted so faintly that nobody can say where it ends — a
//! tail a shade off its background reads as blank and still opens — so a plain
//! URL with any cell under 3:1 earns the same band (`crate::plain_url_at`).
//! Unlike an OSC 8 link's, it shows only while the link modifier is held: a
//! plain URL is found only then, which is when the hand offers its click.
//!
//! It is DISCLOSURE ONLY. Nothing here gates, prompts, or changes what
//! Cmd/ctrl-click opens — the security boundary for that stays `is_safe_url` at
//! the point of action. What this closes is the gap where the only honest
//! account of a link's destination lived in `ctl cell`.
//!
//! # The caption is DERIVED, never remembered
//!
//! What a hover resolution may be kept as is [`LinkHover`] — where the pointer
//! is — and nothing else. The destination, and whether there is a caption at
//! all, are re-read by `App::splice_link_target` from the VIEWPORT it paints,
//! every frame. A URL captured at hover time is a claim about a grid that keeps
//! changing with no pointer event to announce it: the program rewrites the
//! cell, a tab moves to the front, a panel opens over the row. Every one of
//! those is a way for a remembered string to become a lie, and re-deriving is
//! the one seam that closes all of them at once instead of one retirement rule
//! per path.
//!
//! # Why the caption cannot itself deceive
//!
//! The string being displayed came from a hostile program, so the display form
//! is chosen so that no input can make it read as a destination other than the
//! one it is:
//!
//! * **Every byte outside printable ASCII is percent-encoded** ([`safe_form`]).
//!   That is the canonical RFC 3986 spelling of those bytes anyway, and it means
//!   a bidi override, an ASCII or C1 control, a zero-width space, a soft hyphen
//!   and a Cyrillic homoglyph all arrive on screen as visible `%XX` runs instead
//!   of reordering the line, vanishing into it, or impersonating a Latin domain.
//!   The engine already REJECTS OSC 8 URIs carrying controls or bidi overrides
//!   (`handler_osc::handle_osc_8`, CVE-2021-42574); this is the belt at the
//!   presentation end, because the caption must be safe for whatever a future
//!   admission rule lets through.
//! * **Elision never drops the host.** A URL too wide for the band is cut, and
//!   the naive cut — keep the head, append an ellipsis — is exactly the forgery:
//!   `https://google.com.<400 chars>.evil.example/` reads as `google.com`. So
//!   the cut is anchored on the END of the host ([`host_span`]), keeping the
//!   labels that decide which site this is and saying with a leading ellipsis
//!   that something was dropped in front of them.
//! * **The SITE'S OWN LABELS are the emphasized run** — the tail of the host,
//!   not the whole authority ([`emphasis_span`]). The forgery this band exists
//!   to defeat puts its padding INSIDE the authority
//!   (`https://google.com.<400 a's>.evil.example/`), so emphasizing the whole
//!   host emphasizes the padding too and the hierarchy says nothing at all.
//!   Marking only the last labels puts the weight on the answer.
//! * **Userinfo cannot masquerade as the host**: `https://google.com@evil.example/`
//!   emphasizes `evil.example`, because the host is what follows the LAST `@`
//!   of the authority — found the way the browser that opens it finds it, over
//!   the WHOLE URI ([`SCAN_BYTES`]). Userinfo has no length limit, so
//!   `https://google.com.<1000 chars>@evil.example/` carries its host past any
//!   shorter read. Where no host can be found at all, the band shows the scheme
//!   and an ellipsis — never the head standing in for the host.
//!
//! # The seam has an ink of its own, so the caption may have two
//!
//! The band's content-facing edge is an OVERLINE, and a rule that changes ink
//! under the words it passes beneath stops reading as a boundary: a top edge
//! that brightens for the twelve cells above the domain is a highlighter dash
//! floating over them. The seam therefore paints from its OWN channel
//! (`RenderCell::overline_color`, resolved for both backends by
//! `aterm_render::deco_inks`) instead of from each cell's `fg`, so every cell of
//! the row stamps the same edge tone whatever ink it carries.
//!
//! That is what lets the caption answer in BRIGHTNESS as well as weight. The
//! destination host is the one thing a reader came to this row for, so it takes
//! the band's full-contrast tone while the gesture hint, the scheme and the path
//! recede to the secondary one — the ordinary hierarchy of every other chrome
//! band here, which a single-toned row could only imitate with bold.

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
/// Below this many cells for the URL the band paints NOTHING.
///
/// An elided run is `…<text>…`, so under eight cells two of them are spent on
/// the marks and what is left is four or five characters out of the middle of a
/// hostname — a fragment that names no site. The row it would cost belongs to
/// the person's output, and it must buy them more than the underline already
/// gave them.
const MIN_URL_CELLS: usize = 8;

/// How many bytes of a URL are ever read: the engine's `OSC 8` admission cap
/// (`MAX_HYPERLINK_URL_BYTES`), so every URI a program can attach is read
/// whole. No shorter bound is safe — DNS caps a hostname, but the userinfo in
/// front of it has no limit, and a host pushed past the read leaves the cut
/// nothing to anchor on. The read is only a search for the ASCII delimiters
/// [`host_span`] needs; [`safe_form`] spells out just the characters the band
/// paints, so the paint path never expands 8 KiB.
const SCAN_BYTES: usize = 8 * 1024;

const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];

/// The gesture that opens a link, named the way `--help` names it. Cmd on macOS
/// (its native convention), Ctrl everywhere else — the same split
/// `app_mouse::link_modifier_held` makes, so the caption cannot promise a
/// gesture the modifier check does not accept.
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

/// The lead-in for a link whose scheme `crate::is_safe_url` refuses. Saying
/// "opens" there would be the caption's own small lie: the click is a no-op.
const BLOCKED: &str = "blocked scheme";

/// WHERE THE POINTER IS RESTING, when the cell under it carried a destination
/// to disclose the last time the hover was resolved: an OSC 8 hyperlink, or a
/// plain-text URL part of which is painted too faintly to be sure where it
/// ends (`crate::PlainUrl`).
///
/// Deliberately NOT the URL and not a rendered caption: those are re-derived
/// from the live grid at paint (see the module header). What is kept is the
/// only part of a hover that a program, a tab switch or a panel cannot falsify
/// — where the pointer is — plus the identity of the grid it was resolved
/// against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LinkHover {
    /// The PANE-LOCAL `(row, col)` the probe read, in the VIEWPORT rows the
    /// frame is drawn in — the cell `Terminal::hyperlink_at_visible` addresses.
    /// Re-read, not remembered as a URL.
    pub(crate) cell: (u16, u16),
    /// WINDOW terminal row of that cell. The placement may not cover it (a
    /// caption over the text it describes discloses nothing), and it is the row
    /// the pointer is standing on when the splice asks whether chrome has taken
    /// the pointer's own row out from under it.
    pub(crate) window_row: u16,
    /// The session the cell belongs to. A hover is a statement about ONE
    /// terminal's grid, and the front terminal changes under a stationary
    /// pointer for reasons the pointer never hears about — a tab switch, a
    /// pane-focus move, a session migration. The splice re-reads the
    /// destination out of the FRONT terminal and only when it is still this
    /// one, so a hover can never speak for a grid it never looked at.
    pub(crate) session: u64,
}

/// One URL laid out for a band `max_cells` wide: the text to paint and the run
/// within it that is the HOST.
///
/// `host` is measured in CHARACTERS, which for this text is also cells and also
/// columns — every character in it is either printable ASCII or the one-cell
/// ellipsis. It is deliberately not bytes: the ellipsis is three of those, and
/// a byte range would put the emphasis three cells off the run it emphasizes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caption {
    pub(crate) text: String,
    /// The whole host, in characters. What the ELISION anchors on: the labels
    /// that decide which site this is are the last thing the cut may take.
    pub(crate) host: Range<usize>,
}

impl Caption {
    /// The run the band EMPHASIZES: the site's own labels within
    /// [`Self::host`]. See [`emphasis_span`].
    pub(crate) fn emphasis(&self) -> Range<usize> {
        let chars: Vec<char> = self.text.chars().collect();
        emphasis_span(&chars, &self.host)
    }

    /// The text of a run, by the same character measure the runs are expressed
    /// in — the ellipsis is one character and three bytes, so a byte slice
    /// would land three cells off the run it names.
    pub(crate) fn slice(&self, run: &Range<usize>) -> String {
        self.text.chars().skip(run.start).take(run.len()).collect()
    }

    /// The WHOLE host. What the elision proofs measure, since "the host
    /// survived the cut" is the property they are about; the paint path takes
    /// [`Self::emphasis`] instead.
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

/// The run of the host a hurried reader must land on: the site's own labels,
/// never the padding stacked in front of them.
///
/// The last two labels, and a THIRD when the second-to-last is three characters
/// or fewer — the `co.uk` / `com.au` shape, where two labels name a registry
/// rather than a site. Erring long is the safe direction: a run that reaches one
/// label too far still ends at the site that is actually addressed, while one
/// that stops short points at a suffix nobody owns.
///
/// A READING AID, never a boundary. Public-suffix truth needs the PSL, which
/// this deliberately does not carry; what the security argument rests on is that
/// the WHOLE host is always shown and is the last thing elision drops
/// ([`caption`]), not on where the weight falls inside it.
///
/// An address literal (`1.2.3.4`, `[::1]:8443`) has no labels to choose between
/// — every part of it is the identity — so the whole host is the run.
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
    // A FULLY QUALIFIED host ends in the root's empty label — `evil.example.` is
    // a legal, resolvable spelling of `evil.example`. That final dot separates
    // nothing, and counting it shifts every label one place: the run would begin
    // at `example.` and leave `evil.` back at padding weight, which is precisely
    // the forgery this run exists to surface. It is dropped from the count and
    // stays inside the run, because the span still reaches `host.end`.
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
/// Space is encoded too. A URL with a raw space is already refused by
/// `is_safe_url`, and `%20` is what it would have to be spelled as to be one.
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

/// The byte range of the HOST inside a URL — the run that decides which site
/// the link addresses, and therefore the run the band refuses to elide away.
/// Every delimiter it looks for is ASCII, and an ASCII byte never occurs inside
/// a multi-byte UTF-8 sequence, so it reads the raw URL without spelling it
/// out; [`caption`] measures the result in the safe form's characters.
///
/// For a `scheme://` form that is the authority with any `userinfo@` prefix
/// removed (the `@` trick is the oldest way to make a URL read as a site it
/// never touches). For a schemeless-authority form such as `mailto:` it is the
/// recipient, up to the first `?`.
///
/// A WHATWG SPECIAL scheme is read the way the browser that opens it reads it,
/// because that parser — not RFC 3986 — decides where a click lands: any run of
/// `/` and `\` after the colon is skipped (`https:///`, `https:\\` and a bare
/// `https:` all start the authority there), and `\` ends the authority like
/// `/`. `file` is special too, but it has a host grammar of its own and no
/// click opens it, so it keeps the generic reading.
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

/// Lay `url` out for a band `max_cells` wide.
///
/// The cut is the security-relevant part. Keeping the head and appending an
/// ellipsis is the intuitive rule and it is precisely the forgery this band
/// exists to prevent, so when the head does not reach the end of the host the
/// window is anchored on the host's END instead and BOTH ends carry an
/// ellipsis. Whatever the width, the labels that name the site are the last
/// thing to go — and a URL with no host to anchor on shows its scheme alone,
/// because the head is not a host either.
pub(crate) fn caption(url: &str, max_cells: usize) -> Caption {
    let raw = url.trim();
    let mut read = raw.len().min(SCAN_BYTES);
    while !raw.is_char_boundary(read) {
        read -= 1;
    }
    let span = host_span(&raw[..read]);
    // A host is known when it has a character and its authority ENDED inside
    // the read; one still running where the read stopped names nothing yet.
    let known = !span.is_empty() && (span.end < read || read == raw.len());
    // From here on, offsets are the SAFE form's: its characters, its cells.
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
            // No host to anchor on, so nothing is emphasized and nothing past
            // the scheme is shown: whatever follows it would stand where the
            // host belongs. A scheme with a dot is legal and reads as a domain,
            // so only a plain one is named.
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
            // Anchor on the host's END: `…<tail of the host>…`. The TRAILING
            // mark is spent only when something really follows the host — a URL
            // that ends at its host has had nothing dropped after it, and an
            // ellipsis claiming a path that does not exist is the caption
            // telling its own small lie about the destination.
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

/// PURE row builder: exactly `cols` cells, so the splice overwrites one frame
/// row in place. The lead-in names the gesture, the site's own labels are the
/// one emphasized run — full contrast and bold — and everything else recedes to
/// the secondary tone: hierarchy the flat string could not express, exactly as
/// the retired notice card's caption grammar argued for its own. The seam is
/// painted from its own channel rather than from the cells' inks, which is what
/// lets the row carry two of them (see the module header).
///
/// `seam` draws the band's content-facing top edge (a bottom-anchored caption
/// wants it; a caption flipped to the top row is already under the strip's own
/// rule and does not). `tracking` says the program under the pointer tracks
/// the mouse, so the lead-in names the gesture that still opens the link.
///
/// `None` when the band is narrower than [`MIN_URL_CELLS`] leaves for the URL,
/// or when the URL contributes no character at all. A row of the person's
/// output is too expensive to spend on a band that names no site. The decision
/// lives here, with the layout it depends on, so there is one width rule rather
/// than a splice-side guess at this one's terms.
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
    // Re-stamp the emphasized run over the text just written: one `write_str`
    // per role keeps the layout in ONE place, so the emphasis can never land on
    // a different span than the text it is emphasizing. The band's FULL-CONTRAST
    // tone and heavier weight — the destination is the answer this row exists to
    // give, and a reader who takes one thing from a status band takes the
    // brightest thing on it.
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
    // THE SEAM IS THE WHOLE EDGE, AND ONE TONE — closed across the finished row,
    // after every write, in the seam's own ink rather than each cell's. This band
    // stated that rule first and kept it inline; it now lives in `chrome_band` so
    // the paste banner and the find bar close their edges from the same three
    // lines (as the retired config-notice band did), and the message band closes
    // its bottom edge with the twin in `aterm_render::band` (`seal_stack`). See
    // the module header for why the ink is the band's and not the text's.
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

    /// The Trojan Source shape, at the display end. A right-to-left override
    /// inside a URL reorders whatever follows it, so `evil.example` can be made
    /// to read as a trusted domain in any status bar that paints the raw string.
    /// It must reach the band as visible `%XX`, never as a reordering control.
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

    /// Everything that hides, joins or impersonates: C0/C1 controls, a
    /// zero-width space, a soft hyphen, a BOM, and a Cyrillic homoglyph domain
    /// that is pixel-identical to `google.com`. Each one leaves as `%XX`, so
    /// the caption reads as the strange string it is.
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

    /// THE LONG-URL FORGERY. `https://google.com.<padding>.evil.example/` is
    /// wider than any band, and the intuitive cut — keep the head, append an
    /// ellipsis — leaves `https://google.com…` on screen. The cut must keep the
    /// END of the host instead, so the site that is actually addressed is the
    /// last thing to go.
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

    /// The `@` trick: everything before the last `@` is userinfo, not a host.
    /// The emphasized run — the one a hurried reader takes as the destination —
    /// must be `evil.example`.
    #[test]
    fn userinfo_before_an_at_sign_is_never_emphasized_as_the_host() {
        let cap = caption("https://google.com@evil.example/steal", 200);
        assert_eq!(cap.text, "https://google.com@evil.example/steal");
        assert_eq!(cap.host_text(), "evil.example");
    }

    /// THE BROWSER'S AUTHORITY, not RFC 3986's. For `http`/`https` the WHATWG
    /// parser every browser runs skips any run of `/` and `\` after the scheme
    /// and ends the authority at `\` as well as `/ ? #`, so a caption that read
    /// `//` to `/` would bold `google.com` while a click lands on `evil.example`
    /// — or, for `https:///`, find an empty host and fall back to the very
    /// keep-the-head cut the module calls a forgery. Each expectation is what
    /// node's `new URL(u).host` answers.
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

    /// THE WHOLE URI IS READ. Userinfo comes before the host and has no length
    /// limit, so a read that stopped at a fixed count found no host in
    /// `https://google.com.a.a…@evil.example/` and fell back to the keep-the-head
    /// cut the module calls a forgery; a run of slashes the browser skips did
    /// the same with nothing at all. And a URI whose authority outruns even the
    /// whole read names no host, so the band shows its scheme and nothing that
    /// could stand where the host belongs. Each expected host is what node's
    /// `new URL(u).host` answers.
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

    /// A URL that ENDS at its host has nothing after it to drop, so the
    /// host-anchored cut must not append a trailing ellipsis: that mark means
    /// "there was more", and inventing one is the caption misdescribing the
    /// very destination it exists to describe.
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
        // The same URL WITH a path keeps the mark: there really is more.
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

    /// A band too narrow to name a site paints NOTHING. The row it would take
    /// belongs to the person's output, and a few characters cut out of the
    /// middle of a hostname buy them no more knowledge of where the link goes
    /// than the underline already gave them.
    #[test]
    fn a_band_too_narrow_to_name_anything_does_not_take_a_row() {
        let theme = Theme::default();
        // LITERAL widths, not `MARGIN * 2 + MIN_URL_CELLS`: the threshold is the
        // claim under test, so a test that derived its own bounds from the
        // constant would follow it wherever it moved and assert nothing.
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
        // An EMPTY URL contributes no character at any width: the band would be
        // margins and nothing, so it declines the row however wide it is.
        assert!(caption_row("", 200, theme, true, false).is_none());
    }

    /// THE DESTINATION IS THE BRIGHTEST THING ON THE ROW, on every theme the
    /// product ships. Weight is a hierarchy a glance can miss — a bold run in a
    /// dim tone still reads as part of the dim sentence around it — and the one
    /// question this band answers is WHICH SITE. So the site's own labels take
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

    /// THE ATTACK THE MODULE EXISTS FOR, at the presentation end. When the
    /// padding is INSIDE the authority the whole band is host, so emphasizing
    /// "the host" emphasizes the padding too and the hierarchy conveys
    /// nothing — a full row of bold `aaaa…` with the answer hidden in it. The
    /// weight must fall on the site's own labels and on nothing else.
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

    /// Which labels name the SITE. Two of them, or three where two would name a
    /// registry (`co.uk`) instead of a site; an address literal has no labels to
    /// choose between; and userinfo is outside the host to begin with.
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
            // FULLY QUALIFIED spellings. The root's empty label separates
            // nothing, so it must not shift the run one place and hand the
            // padding the site's own name back.
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
