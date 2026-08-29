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
//! `config_notice`/`paste_banner` mechanics (a pure `RenderCell` row builder
//! plus a splice that overwrites one frame row in place) rather than inventing a
//! floating widget of its own.
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
//!   emphasizes `evil.example`, because the host is what follows the LAST `@`.
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
use aterm_render::Theme;

use crate::chrome_band;
use crate::settings::{blank_row, write_str};

/// The caption's left/right margin, in cells (mirrors `config_notice`).
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

/// How many characters of a URL are ever examined. A hostname is capped at 253
/// characters by DNS, so no reachable host can be pushed past this; anything
/// beyond it is path, query or padding, all of which the band elides anyway.
/// The bound is what keeps an 8 KiB `OSC 8` URI (the engine's admission cap)
/// from being expanded and scanned on the paint path.
const SCAN_CHARS: usize = 1024;

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

/// The lead-in for a link whose scheme `crate::is_safe_url` refuses. Saying
/// "opens" there would be the caption's own small lie: the click is a no-op.
const BLOCKED: &str = "blocked scheme";

/// WHERE THE POINTER IS RESTING, when the cell under it carried an OSC 8
/// hyperlink the last time the hover was resolved.
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

/// Re-spell `url` so that nothing in it can reorder, hide inside, or
/// impersonate the rest of the line: printable ASCII survives verbatim, every
/// other character becomes the percent-encoding of its UTF-8 bytes. Returns the
/// safe form and whether the input ran past [`SCAN_CHARS`].
///
/// Space is encoded too. A URL with a raw space is already refused by
/// `is_safe_url`, and `%20` is what it would have to be spelled as to be one.
fn safe_form(url: &str) -> (String, bool) {
    let raw = url.trim();
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw.chars();
    for ch in rest.by_ref().take(SCAN_CHARS) {
        if matches!(ch, '\u{21}'..='\u{7e}') {
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
    (out, rest.next().is_some())
}

/// The range of the HOST inside an already-[`safe_form`]ed URL — the run that
/// decides which site the link addresses, and therefore the run the band refuses
/// to elide away. The safe form is pure ASCII, so its byte offsets are also its
/// character offsets, which is the measure [`Caption::host`] carries.
///
/// For a `scheme://` form that is the authority with any `userinfo@` prefix
/// removed (the `@` trick is the oldest way to make a URL read as a site it
/// never touches). For a schemeless-authority form such as `mailto:` it is the
/// recipient, up to the first `?`.
fn host_span(safe: &str) -> Range<usize> {
    let Some(colon) = safe.find(':') else {
        return 0..safe.len(); // no scheme: the whole thing is the identity
    };
    let after = &safe[colon + 1..];
    if let Some(authority) = after.strip_prefix("//") {
        let start = colon + 3;
        let end = start + authority.find(['/', '?', '#']).unwrap_or(authority.len());
        let host_start = safe[start..end]
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
/// thing to go.
pub(crate) fn caption(url: &str, max_cells: usize) -> Caption {
    let (safe, overlong) = safe_form(url);
    let host = host_span(&safe);
    // A URL cut off before its authority ever terminated cannot be shown to
    // name a host at all, so nothing is emphasized rather than the wrong thing.
    let host = if overlong && host.end == safe.len() {
        0..0
    } else {
        host
    };
    if !overlong && safe.len() <= max_cells {
        return Caption { text: safe, host };
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
        max if host.end < max => {
            // The host ends inside the head, so the ordinary cut is honest.
            let keep = (max - 1).min(safe.len());
            let mut text: String = safe[..keep].to_string();
            text.push(ELLIPSIS);
            let host = host.start.min(keep)..host.end.min(keep);
            Caption { text, host }
        }
        max => {
            // Anchor on the host's END: `…<tail of the host>…`. The TRAILING
            // mark is spent only when something really follows the host — a URL
            // that ends at its host has had nothing dropped after it, and an
            // ellipsis claiming a path that does not exist is the caption
            // telling its own small lie about the destination.
            let after_host = host.end < safe.len();
            let keep = if after_host { max - 2 } else { max - 1 };
            let start = host.end - keep;
            let mut text = String::with_capacity(max);
            text.push(ELLIPSIS);
            text.push_str(&safe[start..host.end]);
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
/// `notice::caption_parts` argues for its own caption grammar. The seam is
/// painted from its own channel rather than from the cells' inks, which is what
/// lets the row carry two of them (see the module header).
///
/// `seam` draws the band's content-facing top edge (a bottom-anchored caption
/// wants it; a caption flipped to the top row is already under the strip's own
/// rule and does not).
///
/// `None` when the band is narrower than [`MIN_URL_CELLS`] leaves for the URL,
/// or when the URL contributes no character at all. A row of the person's
/// output is too expensive to spend on a band that names no site. The decision
/// lives here, with the layout it depends on, so there is one width rule rather
/// than a splice-side guess at this one's terms.
pub(crate) fn caption_row(
    url: &str,
    cols: usize,
    theme: Theme,
    seam: bool,
) -> Option<Vec<RenderCell>> {
    let body = cols.saturating_sub(MARGIN * 2);
    if body < MIN_URL_CELLS {
        return None;
    }
    let c = chrome_band::band_colors(theme);
    let mut row = blank_row(cols, c.label, c.bar_bg, seam);
    let lead = if crate::is_safe_url(url) {
        OPENS
    } else {
        BLOCKED
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
    // THE SEAM IS THE WHOLE EDGE, AND ONE TONE. `write_str` builds each cell it
    // writes from scratch and no chrome text carries an overline of its own, so
    // a seam stamped only by `blank_row` survives exactly where the band happens
    // to have no text: a rule broken into stubs by the words on top of it reads
    // as debris rather than as the band's boundary. Drawn across the finished
    // row, after every write, so no future field can chip it again — and given
    // the seam's OWN ink, so the rule stays one tone across a row whose text
    // deliberately carries two (see the module header).
    if seam {
        for cell in &mut row {
            cell.overline = true;
            cell.overline_color = Some(c.label);
        }
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

    /// A URI whose authority never terminates inside [`SCAN_CHARS`] is not
    /// shown to name any host: emphasizing the arbitrary point the scan stopped
    /// at would invent a destination.
    #[test]
    fn an_authority_longer_than_the_scan_emphasizes_nothing() {
        let url = format!("https://{}", "a".repeat(SCAN_CHARS * 2));
        let cap = caption(&url, 60);
        assert!(cap.host.is_empty(), "{:?}", cap.host);
        assert!(cap.text.ends_with(ELLIPSIS));
        assert_eq!(cap.text.chars().count(), 60);
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
            let row =
                caption_row("https://evil.example/steal", cols, theme, true).expect("a wide band");
            assert_eq!(row.len(), cols, "cols={cols}");
        }
        let row = caption_row("https://evil.example/steal", 80, theme, true).expect("a wide band");
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
        let row =
            caption_row("https://evil.example/steal", 26, theme, true).expect("a narrow band");
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
                caption_row("https://evil.example/steal", cols, theme, true).is_none(),
                "cols={cols} leaves room for `\u{2026}ple\u{2026}` at best, which names \
                 no site, and must not take a row to paint it"
            );
        }
        // Twelve is the first width that shows a recognizable run of the host.
        let row =
            caption_row("https://evil.example/steal", 12, theme, true).expect("12 cells suffice");
        let seen: String = text_of(&row).chars().filter(|c| *c != ' ').collect();
        assert_eq!(seen, "…xample…", "{seen:?}");
        let row = caption_row("https://evil.example/steal", 16, theme, true)
            .expect("16 cells carry a run of the host");
        let text = text_of(&row);
        assert!(
            text.chars().any(|ch| ch.is_ascii_alphanumeric()),
            "{text:?}"
        );
        // An EMPTY URL contributes no character at any width: the band would be
        // margins and nothing, so it declines the row however wide it is.
        assert!(caption_row("", 200, theme, true).is_none());
    }

    /// The seam is the band's content-facing EDGE, so it must run the whole
    /// width. Every cell carries it — the margins, the lead-in, the gap, the
    /// URL and the tail alike — or the rule reads as scattered debris over the
    /// row rather than as a boundary.
    #[test]
    fn the_seam_runs_unbroken_and_one_toned_across_every_cell_of_the_band() {
        let theme = Theme::default();
        let row = caption_row("https://evil.example/steal", 80, theme, true).expect("a wide band");
        assert!(
            row.iter().all(|cell| cell.overline),
            "the seam breaks at columns {:?}",
            row.iter()
                .enumerate()
                .filter(|(_, cell)| !cell.overline)
                .map(|(col, _)| col)
                .collect::<Vec<_>>()
        );
        // AND IT IS ONE TONE, stated where the seam is actually drawn from. The
        // row deliberately carries TWO inks (the host is brighter than the rest),
        // so a seam left to each cell's `fg` would put a brighter dash over
        // exactly the cells above the domain — the whole reason the edge has a
        // colour channel of its own.
        let seams: std::collections::BTreeSet<Option<[u8; 3]>> =
            row.iter().map(|cell| cell.overline_color).collect();
        assert_eq!(
            seams.len(),
            1,
            "the rule takes as many tones as the band has inks: {seams:?}"
        );
        assert!(
            seams.iter().all(Option::is_some),
            "an uncoloured seam falls back to its cell's ink: {seams:?}"
        );
        let inks: std::collections::BTreeSet<[u8; 3]> = row.iter().map(|cell| cell.fg).collect();
        assert!(
            inks.len() > 1,
            "the band is meant to carry hierarchy in ink, so this proof is not vacuous: {inks:?}"
        );
        // A caption flipped to the top row sits under the tab strip's own rule,
        // so it asks for no second one anywhere.
        let row = caption_row("https://evil.example/steal", 80, theme, false).expect("a wide band");
        assert!(row.iter().all(|cell| !cell.overline));
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
            let parts = aterm_types::scheme::builtin(name)
                .expect("listed scheme exists")
                .to_theme_parts();
            let theme = Theme {
                fg: parts.fg,
                bg: parts.bg,
                cursor: parts.cursor,
                selection: parts.selection,
            };
            let c = chrome_band::band_colors(theme);
            assert_ne!(
                c.value, c.label,
                "{name}: two tones or there is no hierarchy to assert"
            );
            let row = caption_row("https://google.com@evil.example/steal", 80, theme, true)
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
        let row = caption_row(&url, 64, Theme::default(), true).expect("a wide band");
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
        let row =
            caption_row("file:///etc/passwd", 80, Theme::default(), true).expect("a wide band");
        let text = text_of(&row);
        assert!(text.contains(BLOCKED), "{text:?}");
        assert!(!text.contains(OPENS), "{text:?}");
        assert!(text.contains("file:///etc/passwd"), "{text:?}");
    }
}
