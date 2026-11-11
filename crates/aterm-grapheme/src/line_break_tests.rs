// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn pieces(text: &str, keep: fn(BreakKind) -> bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    for brk in line_breaks(text).into_iter().filter(|brk| keep(brk.kind)) {
        out.push(text[from..brk.at].trim());
        from = brk.at;
    }
    out.push(text[from..].trim());
    out
}

fn all(_: BreakKind) -> bool {
    true
}

#[test]
fn latin_breaks_at_spaces_and_inside_prose_words() {
    assert_eq!(
        pieces("Cursor blink was not saved", all),
        ["Cursor", "blink", "was", "not", "saved"]
    );
    assert_eq!(
        pieces("a built-in and/or (re-enabled)", all),
        ["a", "built-", "in", "and/", "or", "(re-", "enabled)"]
    );
    assert_eq!(pieces("50\u{a0}% done", all), ["50\u{a0}%", "done"]);
    assert_eq!(pieces("  lead  and trail  ", all), ["lead", "and", "trail"]);
    assert!(line_breaks("").is_empty());
    assert!(line_breaks("word").is_empty());
    assert_eq!(pieces("(a) b, c!", all), ["(a)", "b,", "c!"]);
    assert_eq!(pieces("CPU / GPU / RAM", all), ["CPU /", "GPU /", "RAM"]);
    assert_eq!(pieces("wait ! now", all), ["wait !", "now"]);
}

#[test]
fn a_path_breaks_before_its_slashes_and_an_identifier_nowhere_else() {
    assert_eq!(
        pieces("saved: /private/tmp/lab-42/75d5bccd-9c38/x", all),
        [
            "saved:",
            "/private",
            "/tmp",
            "/lab-42",
            "/75d5bccd-9c38",
            "/x"
        ]
    );
    assert_eq!(
        pieces("aterm -e /bin/sh --columns 80", all),
        ["aterm", "-e", "/bin", "/sh", "--columns", "80"]
    );
    assert_eq!(
        pieces("~/.config/aterm/aterm.toml", all),
        ["~/.config", "/aterm", "/aterm.toml"]
    );
    assert_eq!(
        pieces("https://github.com/alabsystems/aterm", all),
        ["https://", "github.com", "/alabsystems", "/aterm"]
    );
    assert_eq!(
        pieces("aarch64-apple-darwin MacBook-Pro-M4 Applications/", all),
        ["aarch64-apple-darwin", "MacBook-Pro-M4", "Applications/"]
    );
    assert_eq!(pieces(r"C:\Users\me", all), [r"C:\Users", r"\me"]);
    assert_eq!(
        pieces("Application Support/aterm/pkg", all),
        ["Application", "Support", "/aterm", "/pkg"]
    );
    for (text, kind) in [
        ("Brightness", TokenKind::Word),
        ("AUTOMATIC", TokenKind::Word),
        ("Messages.", TokenKind::Word),
        ("(default)", TokenKind::Word),
        ("Choose\u{2026}", TokenKind::Word),
        ("Couldn't", TokenKind::Word),
        ("\u{b7}", TokenKind::Word),
        ("alab.systems", TokenKind::Identifier),
        ("0.11.0+g20565c325", TokenKind::Identifier),
        ("/private", TokenKind::Identifier),
        ("macOS", TokenKind::Identifier),
        ("pre-", TokenKind::Identifier),
        ("and/or", TokenKind::Word),
        ("Support/aterm/pkg", TokenKind::Identifier),
    ] {
        assert_eq!(token_kind(text), kind, "{text:?}");
    }
}

fn inside(text: &str, mark: &str, width: usize) -> Vec<String> {
    let fits = |line: &str| line.chars().count() <= width;
    break_inside(text, 0..text.len(), mark, &fits)
        .into_iter()
        .map(|(at, line)| {
            let plain = line
                .strip_suffix('-')
                .filter(|_| !mark.is_empty())
                .unwrap_or(&line);
            assert!(text[at..].starts_with(plain), "{text:?}: {line:?} at {at}");
            line
        })
        .collect()
}

#[test]
fn a_piece_wider_than_its_line_breaks_where_a_typesetter_would() {
    assert_eq!(inside("Brightness", "-", 8), ["Bright-", "ness"]);
    assert_eq!(inside("AUTOMATIC", "-", 7), ["AUTO-", "MATIC"]);
    assert_eq!(inside("notifications", "-", 10), ["notifica-", "tions"]);
    assert_eq!(inside("Messages.", "-", 7), ["Mes-", "sages."]);
    assert_eq!(inside("SUPERVISION", "-", 9), ["SUPERVI-", "SION"]);
    assert_eq!(inside("COMPUTER \u{b7}", "-", 7), ["COM-", "PUTER \u{b7}"]);
    assert_eq!(inside("alab.systems", "-", 9), ["alab", ".systems"]);
    assert_eq!(
        inside("0.11.0+g20565c325", "-", 12),
        ["0.11.0", "+g20565c325"]
    );
    assert_eq!(
        inside("ec73ab1c007d90ca", "-", 12),
        ["ec73ab1c007d", "90ca"]
    );
    assert_eq!(inside("aterm.toml", "-", 7), ["aterm", ".toml"]);
    assert_eq!(inside("/Applications", "-", 10), ["/Applica", "tions"]);
    assert_eq!(inside("ai-fleet.toml", "-", 7), ["ai-", "fleet", ".toml"]);
    assert_eq!(
        inside("/-Users-someone/75d5bccd-9c38-431b", "-", 10),
        ["/-Users-", "someone", "/75d5bccd-", "9c38-431b"]
    );
    assert_eq!(
        inside("OpenAI-compatible", "-", 10),
        ["OpenAI-", "compatible"]
    );
    assert_eq!(inside("build-501x", "-", 8), ["build-", "501x"]);
    assert_eq!(inside("Pro-3.1GHz-i7", "-", 10), ["Pro-", "3.1GHz-i7"]);
    assert_eq!(inside("192.168.1.114", "-", 10), ["192.168.1", ".114"]);
    assert_eq!(inside("Choose\u{2026}", "-", 6), ["Choo-", "se\u{2026}"]);
    assert_eq!(inside("STRENGTHS", "-", 8), ["STRENG-", "THS"]);
    let fits = |line: &str| line.chars().count() <= 10;
    let after = |lead: &'static str| move |part: &str| fits(&format!("{lead}{part}"));
    assert_eq!(
        break_into_line("translucent", 0..11, "-", &after("a "), &fits),
        Some((7, "translu-".to_string()))
    );
    assert_eq!(
        break_into_line("Choose", 0..6, "-", &after("a b c "), &fits),
        None
    );
    let wide = |line: &str| line.chars().count() <= 12;
    let version = "0.11.0+g20565c325";
    assert_eq!(
        break_into_line(
            version,
            0..17,
            "-",
            &|part| wide(&format!("Trust {part}")),
            &wide
        ),
        Some((6, "0.11.0".to_string()))
    );
    assert_eq!(
        break_into_line(version, 0..17, "-", &after("Trust "), &fits),
        None
    );
    assert_eq!(inside("background", "", 6), ["back", "ground"]);
    assert_eq!(inside("cursor_style", "", 7), ["cursor", "_style"]);
    assert_eq!(inside("a", "-", 0), ["a"]);
    assert_eq!(inside("ab", "-", 0), ["a-", "b"]);
    assert_eq!(
        break_inside("a \u{b7}", 0..4, "-", &|line| line.chars().count() <= 1),
        [(0, "a".to_string()), (1, "\u{b7}".to_string())]
    );
    // Out-of-bounds, inverted, or mid-UTF-8 piece ranges clamp cleanly instead of panicking.
    let multibyte = "設定";
    let inverted = core::ops::Range { start: 5, end: 2 };
    assert_eq!(
        break_inside(multibyte, 1..99, "-", &|_| false),
        [(0, "設-".to_string()), (3, "定".to_string())]
    );
    assert_eq!(
        break_inside(multibyte, inverted.clone(), "-", &|_| false),
        [(3, String::new())]
    );
    assert_eq!(
        break_into_line(multibyte, inverted, "-", &|_| true, &|_| true),
        None
    );
}
/// A typesetter breaks a name AFTER its hyphens (the hyphen ends the line, never starts
/// the next) and keeps a file's extension with the word before it.
#[test]
fn a_name_breaks_after_its_hyphens_and_keeps_its_extension() {
    for (text, width, lines) in [
        (
            "sunset-over-the-bay.png",
            16,
            &["sunset-over-the-", "bay.png"][..],
        ),
        (
            "sunset-over-the-bay.png",
            20,
            &["sunset-over-the-", "bay.png"],
        ),
        ("ai-fleet.toml", 10, &["ai-", "fleet.toml"]),
        ("/aterm-r14-br2", 11, &["/aterm-r14-", "br2"]),
        (
            "75d5bccd-9c38-431b-99e5-ef81935c6fbd",
            15,
            &["75d5bccd-9c38-", "431b-99e5-", "ef81935c6fbd"],
        ),
        (
            "user-name@mail-host.example",
            16,
            &["user-name@mail-", "host.example"],
        ),
    ] {
        let broken = inside(text, "-", width);
        assert_eq!(broken, lines, "{text:?} in {width}");
        for line in &broken[1..] {
            assert!(!line.starts_with(['-', '.']), "{text:?}: {broken:?}");
        }
    }
    // Only where a word and its hyphen are wider than the line does the hyphen start the
    // next line, rather than the word being cut in two ("/ate" / "rm-lab").
    assert_eq!(inside("/aterm-lab", "-", 6), ["/aterm", "-lab"]);
    assert_eq!(inside("/75d5bccd-9c38", "-", 9), ["/75d5bc", "cd-9c38"]);
    assert_eq!(
        inside("m1-MacBook-Air", "-", 7),
        ["m1-", "Mac", "Book-", "Air"]
    );
    assert_eq!(
        inside("/aterm-lab-instance-runner", "-", 6),
        ["/aterm", "-lab-", "in", "stance", "-run", "ner"]
    );
}

/// A word no break of the usual kind fits (three letters after the hyphen) still breaks
/// where a syllable ends, two letters from its end, as a narrow column sets it: never cut
/// where no syllable ends ("recor-" / "ded", "men-" / "ded").
#[test]
fn a_narrow_measure_keeps_a_word_to_its_syllables() {
    assert_eq!(inside("recorded", "-", 7), ["record-", "ed"]);
    assert_eq!(
        inside("Recommended", "-", 5),
        ["Rec-", "om-", "mend-", "ed"]
    );
    // The usual breaks come first wherever one fits.
    assert_eq!(inside("automatically", "-", 12), ["automati-", "cally"]);
    assert_eq!(inside("Recommended", "-", 10), ["Recom-", "mended"]);
    // A word with no syllable to break at is still cut where its line ends, and never
    // carries one letter alone over ("Ope-" / "n" where the mark costs no room).
    assert_eq!(inside("Changed", "-", 5), ["Chan-", "ged"]);
    let three_letters = |line: &str| line.trim_end_matches('-').chars().count() <= 3;
    let open: Vec<String> = break_inside("Open", 0..4, "-", &three_letters)
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    assert_eq!(open, ["Op-", "en"]);
}

#[test]
fn cjk_breaks_between_characters_with_kinsoku() {
    assert_eq!(
        pieces("設定を保存できません。", all),
        ["設", "定", "を", "保", "存", "で", "き", "ま", "せ", "ん。"]
    );
    assert_eq!(
        pieces("「設定」を開く", all),
        ["「設", "定」", "を", "開", "く"]
    );
    assert_eq!(pieces("ちょっと", all), ["ちょっ", "と"]);
    assert_eq!(pieces("コーヒー", all), ["コー", "ヒー"]);
    assert_eq!(pieces("한국어 설정", all), ["한", "국", "어", "설", "정"]);
    assert_eq!(pieces("か\u{3099}き", all), ["か\u{3099}", "き"]);
    assert_eq!(pieces("aterm設定。", all), ["aterm", "設", "定。"]);
    assert_eq!(pieces("設定 。次", all), ["設", "定 。", "次"]);
    assert_eq!(pieces("終」「始", all), ["終」", "「始"]);
}

#[test]
fn pieces_are_what_a_wrap_lays_down() {
    fn words(text: &str, keep: fn(BreakKind) -> bool) -> Vec<(bool, &str)> {
        super::pieces(text, keep)
            .into_iter()
            .map(|piece| {
                assert_eq!(&text[piece.at..piece.at + piece.text.len()], piece.text);
                (piece.spaced, piece.text)
            })
            .collect()
    }
    for text in [
        "Show the tab bar when only one tab is open",
        "  lead  and\ttrail \n",
        "aarch64-apple-darwin ~/a/b built-in",
        "",
        "word",
    ] {
        let split: Vec<(bool, &str)> = text
            .split_whitespace()
            .enumerate()
            .map(|(n, word)| (n > 0, word))
            .collect();
        assert_eq!(words(text, BreakKind::is_word_boundary), split, "{text:?}");
    }
    assert_eq!(
        words("built-in x", all),
        [(false, "built-"), (false, "in"), (true, "x")]
    );
    assert_eq!(
        words("設定「テーマ」を開く。 next", BreakKind::is_word_boundary),
        [
            (false, "設"),
            (false, "定"),
            (false, "「テー"),
            (false, "マ」"),
            (false, "を"),
            (false, "開"),
            (false, "く。"),
            (true, "next"),
        ]
    );
    assert_eq!(
        words("50\u{a0}% done", all),
        [(false, "50\u{a0}%"), (true, "done")]
    );
    assert!(is_break_space(' ') && is_break_space('\u{3000}'));
    assert!(!is_break_space('\u{a0}') && !is_break_space('a'));
}

#[test]
fn word_breaks_are_spaces_and_cjk() {
    let words =
        |text: &str| -> Vec<usize> { word_breaks(text).into_iter().map(|brk| brk.at).collect() };
    assert_eq!(words("use your-theme now"), [4, 15]);
    assert_eq!(words("設定です"), [3, 6, 9]);
    assert!(word_breaks("a/b-c").is_empty());
}
