// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn text_of(row: &[RenderCell]) -> String {
    row.iter().map(|cell| cell.ch).collect()
}

/// Trojan Source at the display end: a right-to-left override reaches the band as `%XX`.
#[test]
fn a_bidi_override_is_spelled_out_instead_of_reordering_the_caption() {
    let cap = caption("https://safe.example\u{202e}moc.live/x", 200);
    assert!(
        !cap.text
            .chars()
            .any(|c| c.is_control()
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
    let row =
        caption_row("https://evil.example/steal", 26, theme, true, false).expect("a narrow band");
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
        let theme = Theme::from(&aterm_types::scheme::builtin(name).expect("listed scheme exists"));
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
    let row =
        caption_row("file:///etc/passwd", 80, Theme::default(), true, false).expect("a wide band");
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
    let row =
        caption_row("file:///etc/passwd", 80, Theme::default(), true, true).expect("a wide band");
    assert!(text_of(&row).contains(BLOCKED));
}

/// A file citation's band says where the click goes and that it stays in
/// aterm: `opens in aterm: src/foo.rs:42`, the file name and line the
/// emphasized run, and never the OS gesture's `opens`/`blocked` words.
#[test]
fn a_file_citation_caption_names_aterm_and_the_line() {
    let row = path_caption_row("src/foo.rs:42", 80, Theme::default(), true, false).expect("a band");
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
