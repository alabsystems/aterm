// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A NEEDS ATTENTION ROW SAYS WHERE, THEN WHAT, IN A ROW'S FEW WORDS: each
/// way the parser refuses a theme file reads as its line, its key and the
/// trouble, never the parser's preamble; another terminal's theme says
/// Import converts it, then its format.
#[test]
fn a_load_failure_reads_as_its_line_and_key() {
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    let base = "foreground = #dddddd\nbackground = #101010\n";
    let files = [
        (
            "a",
            format!("{base}cursor = #zzz\n"),
            "Line 3: cursor isn\u{2019}t a color",
        ),
        (
            "b",
            format!("{base}wibble = #123456\n"),
            "Line 3: wibble is no theme key",
        ),
        (
            "c",
            format!("{base}cursor =\n"),
            "Line 3: cursor has no value",
        ),
        ("d", format!("{base}cursor\n"), "Line 3: no = on the line"),
        (
            "e",
            format!("{base}background = #000000\n"),
            "Line 3: background is set twice",
        ),
        (
            "f",
            format!("{base}color1 = #ff0000\ncolor1 = #00ff00\n"),
            "Line 4: color 1 is set twice",
        ),
        ("g", "background = #101010\n".to_string(), "No foreground"),
        (
            "h",
            "{\"background\": \"#101010\"}\n".to_string(),
            "Import\u{2026} converts it (JSON)",
        ),
        (
            "i",
            format!("{base}appearance = sideways\n"),
            "Line 3: appearance must be dark|light, got \"sideways\"",
        ),
    ];
    for (name, text, _) in &files {
        std::fs::write(dir.path().join(format!("{name}.conf")), text).unwrap();
    }
    let rows = attention_rows(
        &crate::app_config::ThemeCatalog::discover_in(dir.path()),
        &[],
    );
    for (name, _, summary) in files {
        let row = rows
            .iter()
            .find(|row| row.id == name)
            .unwrap_or_else(|| panic!("{name} needs attention"));
        assert_eq!(row.summary, summary, "{name}: {}", row.reason);
        assert!(
            row.reason.starts_with("theme parse error on line "),
            "{name}"
        );
    }
}

/// Signature themes lead, then every other built-in (Default among
/// them), then the person's themes by name, then the community pack's;
/// sections never interleave, and every slug is unique and key-safe.
#[test]
fn showcase_cards_put_signature_themes_first() {
    let mine = |name: &str, bg: Rgb| {
        let mut scheme = ColorScheme {
            name: name.to_string(),
            background: bg,
            ..ColorScheme::default()
        };
        scheme.appearance = Appearance::Dark;
        (name.to_string(), scheme)
    };
    let themes = crate::app_config::ThemeCatalog::from_schemes([
        mine("zebra", Rgb::new(1, 2, 3)),
        mine("Alpine Night", Rgb::new(4, 5, 6)),
    ]);
    let cards = showcase_cards(&themes);
    let signature: Vec<&str> = aterm_types::scheme::builtin_theme_list()
        .iter()
        .filter(|builtin| builtin.signature)
        .map(|builtin| builtin.name)
        .collect();
    assert!(!signature.is_empty(), "aterm ships signature themes");
    let leading: Vec<&str> = cards
        .iter()
        .take(signature.len())
        .map(|card| card.id.as_str())
        .collect();
    assert_eq!(leading, signature);
    let tiers: Vec<Section> = cards.iter().map(|card| card.section).collect();
    let mut sorted = tiers.clone();
    sorted.sort();
    assert_eq!(tiers, sorted, "sections are contiguous, in list order");
    assert!(
        cards
            .iter()
            .any(|card| card.id == "Default" && card.section == Section::Builtin)
    );
    let yours: Vec<&str> = cards
        .iter()
        .filter(|card| card.section == Section::Yours)
        .map(|card| card.id.as_str())
        .collect();
    assert_eq!(yours, ["Alpine Night", "zebra"]);
    let mut slugs = std::collections::BTreeSet::new();
    for card in &cards {
        assert!(
            card.slug
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
            "{}",
            card.slug
        );
        assert!(slugs.insert(card.slug.clone()), "duplicate {}", card.slug);
    }
    assert_eq!(
        cards
            .iter()
            .filter(|card| card.section < Section::Yours)
            .count(),
        aterm_types::scheme::builtin_theme_list().len()
    );
    assert_eq!(
        cards
            .iter()
            .filter(|card| card.section == Section::Community)
            .count(),
        aterm_theme_pack::themes().len()
    );
}

/// A row's value leads with its appearance and its contrast figure and
/// grade, rounded down as the figure always is, and says in use and
/// trying only when they hold.
#[test]
fn a_rows_value_reads_its_appearance_contrast_and_state() {
    let cards = showcase_cards(&crate::app_config::ThemeCatalog::empty());
    for card in &cards {
        let value = card.row_value(false, false);
        let (word, rest) = value.split_once(", contrast ").expect("contrast");
        assert_eq!(word, card.appearance_word());
        let (figure, grade) = rest.split_once(":1 ").expect(":1");
        assert_eq!(figure, contrast_figure(card.contrast));
        assert!(grade.starts_with(contrast_grade(card.contrast).word()));
        assert!(!value.contains("in use") && !value.contains("trying"));
        let both = card.row_value(true, true);
        assert!(
            both.contains(", in use") && both.contains(", trying"),
            "{both}"
        );
    }
}

/// The slab paints the row's own colours: its background fills the slab,
/// "Aa" is inked in its foreground, and its four hues are dots — for
/// three themes of different looks.
#[test]
fn the_row_slab_paints_the_cards_own_colours() {
    let cards = showcase_cards(&crate::app_config::ThemeCatalog::empty());
    let roles = Roles::from_theme(aterm_render::Theme::default());
    let rect = LogicalRect::new(0.0, 0.0, SLAB_WIDTH, SLAB_HEIGHT);
    let painted = |value: &str| {
        let mut prims = Vec::new();
        paint_row_slab(&mut prims, rect, Some(value), &roles);
        let fill = prims.iter().find_map(|prim| match prim {
            DrawPrim::Panel { fill, w, .. } if *w == SLAB_WIDTH => Some(*fill),
            _ => None,
        });
        let ink = prims.iter().find_map(|prim| match prim {
            DrawPrim::Text { s, color, .. } if s == "Aa" => Some(*color),
            _ => None,
        });
        let dots: Vec<_> = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel { fill, w, .. } if *w < 10.0 => Some(*fill),
                _ => None,
            })
            .collect();
        (fill, ink, dots)
    };
    let picks = ["Aterm Night", "Aterm Day", "Dracula"];
    for id in picks {
        let card = cards.iter().find(|card| card.id == id).expect("a built-in");
        let (fill, ink, dots) = painted(&card.slab_value(false));
        assert_eq!(fill, Some(rgba(unpack(card.bg), 255)), "{id}: background");
        assert_eq!(ink, Some(rgba(unpack(card.fg), 255)), "{id}: Aa");
        assert_eq!(
            dots,
            [1, 2, 4, 5].map(|slot| rgba(unpack(card.ansi[slot]), 255)),
            "{id}: hues"
        );
        let (_, _, narrow) = painted(&card.slab_value(true));
        assert_eq!(narrow.len(), 2, "{id}: a narrow slab dots two hues");
        let mut swapped = card.clone();
        std::mem::swap(&mut swapped.fg, &mut swapped.bg);
        let (fill, ink, _) = painted(&swapped.slab_value(false));
        assert_ne!(fill, Some(rgba(unpack(card.bg), 255)), "{id}: swapped");
        assert_ne!(ink, Some(rgba(unpack(card.fg), 255)), "{id}: swapped");
    }
    let mut prims = Vec::new();
    paint_row_slab(&mut prims, rect, Some("not a slab"), &roles);
    assert!(prims.is_empty(), "a malformed value paints nothing");
}

#[test]
fn contrast_figures_round_down_and_grade_by_wcag() {
    assert_eq!(contrast_figure(2.96), "2.9");
    assert_eq!(contrast_figure(7.0), "7");
    assert_eq!(contrast_figure(11.24), "11.2");
    assert_eq!(contrast_grade(7.0), ContrastGrade::Aaa);
    assert_eq!(contrast_grade(6.99), ContrastGrade::Aa);
    assert_eq!(contrast_grade(4.5), ContrastGrade::Aa);
    assert_eq!(contrast_grade(4.49), ContrastGrade::Low);
}
