// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn rgb(hex: u32) -> Rgb {
    Rgb::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn expected(
    name: &str,
    fg: u32,
    bg: u32,
    cursor: Option<u32>,
    selection: Option<u32>,
    selection_fg: Option<u32>,
    ansi: [u32; 16],
) -> ColorScheme {
    ColorScheme {
        name: name.to_string(),
        appearance: Appearance::Dark,
        foreground: rgb(fg),
        background: rgb(bg),
        cursor: cursor.map(rgb),
        selection: selection.map(rgb),
        selection_fg: selection_fg.map(rgb),
        ansi: ansi.map(rgb),
        chrome: ChromeColors::default(),
        palette_generate: false,
    }
}

const PALETTE: [u32; 16] = [
    0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868,
    0xff899d, 0x9fe044, 0xfaba4a, 0x8db0ff, 0xc7a9ff, 0xa4daff, 0xc0caf5,
];

fn assert_round_trips(report: &ImportReport) {
    let text = report.to_native_conf(&["Imported by a test", "from \u{7}a bell"]);
    let parsed = parse_theme_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(
        parsed,
        ParsedTheme::colours_only(
            report.scheme.clone(),
            report.appearance_declared,
            report.description.clone(),
            report.author.clone(),
        ),
        "{text}"
    );
}

fn kitty_sample() -> String {
    let mut text = String::from(
        "# vim:ft=kitty\n\
         ## name: Tokyo Night\n\
         ## author: enkia\n\
         ## blurb: A clean dark theme\n\
         foreground #c0caf5\n\
         background #1a1b26\n\
         cursor     #c0caf5\n\
         selection_background #283457\n\
         selection_foreground none\n\
         url_color #73daca\n\
         active_tab_background #7aa2f7\n\
         color16 #ff9e64\n\
         include ../extra.conf\n",
    );
    for (index, colour) in PALETTE.iter().enumerate() {
        text.push_str(&format!("color{index} #{colour:06x}\n"));
    }
    text
}

#[test]
fn kitty_converts_structurally() {
    let report = convert(ImportFormat::Kitty, &kitty_sample(), None).unwrap();
    let mut want = expected(
        "Tokyo Night",
        0xc0caf5,
        0x1a1b26,
        Some(0xc0caf5),
        Some(0x283457),
        None,
        PALETTE,
    );
    want.chrome.active_tab = Some(rgb(0x7aa2f7));
    assert_eq!(report.scheme, want);
    assert_eq!(report.author.as_deref(), Some("enkia"));
    assert_eq!(report.description.as_deref(), Some("A clean dark theme"));
    assert_eq!(report.dropped, ["url_color", "color16", "include"]);
    assert!(report.notes.iter().any(|n| n.contains("not followed")));
    assert_round_trips(&report);
}

#[test]
fn ghostty_converts_structurally() {
    let mut text = String::from(
        "background = 1a1b26\n\
         foreground = #c0caf5\n\
         cursor-color = #c0caf5\n\
         cursor-text = #1a1b26\n\
         selection-background = #283457\n\
         selection-foreground = #c0caf5\n\
         palette = 200=#123456\n\
         config-file = other\n\
         font-family = Iosevka\n",
    );
    for (index, colour) in PALETTE.iter().enumerate() {
        text.push_str(&format!("palette = {index}=#{colour:06x}\n"));
    }
    let report = convert(ImportFormat::Ghostty, &text, None).unwrap();
    assert_eq!(
        report.scheme,
        expected(
            "",
            0xc0caf5,
            0x1a1b26,
            Some(0xc0caf5),
            Some(0x283457),
            Some(0xc0caf5),
            PALETTE
        )
    );
    assert_eq!(
        report.dropped,
        ["cursor-text", "palette 200", "config-file", "font-family"]
    );
    assert_round_trips(&report);
}

#[test]
fn ghostty_import_carries_palette_generate() {
    let base = "background = #1a1b26\nforeground = #c0caf5\n";
    let on = convert(
        ImportFormat::Ghostty,
        &format!("{base}palette-generate = true\n"),
        None,
    )
    .unwrap();
    assert!(on.scheme.palette_generate);
    assert!(on.dropped.is_empty(), "{:?}", on.dropped);
    assert_round_trips(&on);
    let bad = convert(
        ImportFormat::Ghostty,
        &format!("{base}palette-generate = maybe\n"),
        None,
    )
    .unwrap();
    assert!(!bad.scheme.palette_generate);
    assert_eq!(bad.dropped, ["palette-generate"]);
}

#[test]
fn chrome_keys_import_from_their_foreign_names() {
    let ghostty = "background = #1a1b26\n\
                   foreground = #c0caf5\n\
                   cursor-text = #1a1b26\n\
                   split-divider-color = #3b4261\n";
    let report = convert(ImportFormat::Ghostty, ghostty, None).unwrap();
    assert_eq!(report.scheme.chrome.border, Some(rgb(0x3b4261)));
    assert_eq!(report.dropped, ["cursor-text"]);
    assert_round_trips(&report);
    let alacritty = "[colors.primary]\n\
                     background = \"#1a1b26\"\n\
                     foreground = \"#c0caf5\"\n\
                     [colors.search.matches]\n\
                     foreground = \"#1a1b26\"\n\
                     background = \"#3d4f2f\"\n";
    let report = convert(ImportFormat::Alacritty, alacritty, None).unwrap();
    assert_eq!(report.scheme.chrome.search_match, Some(rgb(0x3d4f2f)));
    assert_eq!(report.dropped, ["colors.search.matches.foreground"]);
    assert_round_trips(&report);
}

#[test]
fn alacritty_converts_structurally() {
    let names = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let mut text = String::from(
        "# Tokyo Night\n\
         import = [\n  \"~/.config/alacritty/base.toml\",\n]\n\
         [window]\nopacity = 0.9\n\
         [colors.primary]\n\
         background = '#1a1b26' # trailing comment\n\
         foreground = \"0xc0caf5\"\n\
         dim_foreground = \"#a9b1d6\"\n\
         [colors.cursor]\n\
         cursor = \"#c0caf5\"\n\
         text = \"CellBackground\"\n\
         [colors]\n\
         selection = { background = \"#283457\", text = \"CellForeground\" }\n",
    );
    for (group, offset) in [("normal", 0), ("bright", 8)] {
        text.push_str(&format!("[colors.{group}]\n"));
        for (index, name) in names.iter().enumerate() {
            text.push_str(&format!("{name} = \"#{:06x}\"\n", PALETTE[index + offset]));
        }
    }
    let report = convert(ImportFormat::Alacritty, &text, None).unwrap();
    assert_eq!(
        report.scheme,
        expected(
            "",
            0xc0caf5,
            0x1a1b26,
            Some(0xc0caf5),
            Some(0x283457),
            None,
            PALETTE
        )
    );
    assert_eq!(
        report.dropped,
        [
            "import",
            "window.opacity",
            "colors.primary.dim_foreground",
            "colors.cursor.text"
        ]
    );
    assert_round_trips(&report);
}

#[test]
fn alacritty_reports_selection_text_that_cannot_follow_the_cell_background() {
    for (value, dropped) in [("CellForeground", false), ("CellBackground", true)] {
        let text = format!(
            "[colors.primary]\nforeground = '#c0caf5'\nbackground = '#1a1b26'\n\
             [colors.selection]\ntext = '{value}'\n"
        );
        let report = convert(ImportFormat::Alacritty, &text, None).unwrap();
        assert_eq!(report.scheme.selection_fg, None);
        assert_eq!(
            report
                .dropped
                .iter()
                .any(|key| key == "colors.selection.text"),
            dropped,
            "{value}"
        );
        assert_eq!(report.notes.len(), if dropped { 2 } else { 1 }, "{value}");
        assert_round_trips(&report);
    }
}

fn iterm_colour_dict(hex: u32, space: &str) -> String {
    let c = rgb(hex);
    format!(
        "<dict>\n<key>Alpha Component</key><real>1</real>\n\
         <key>Blue Component</key><real>{}</real>\n\
         <key>Color Space</key><string>{space}</string>\n\
         <key>Green Component</key><real>{}</real>\n\
         <key>Red Component</key><real>{}</real>\n</dict>\n",
        f64::from(c.b) / 255.0,
        f64::from(c.g) / 255.0,
        f64::from(c.r) / 255.0,
    )
}

#[test]
fn iterm_converts_structurally() {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n<!-- generated -->\n",
    );
    for (index, colour) in PALETTE.iter().enumerate() {
        text.push_str(&format!("<key>Ansi {index} Color</key>\n"));
        text.push_str(&iterm_colour_dict(*colour, "sRGB"));
    }
    for (key, colour) in [
        ("Background Color", 0x1a1b26),
        ("Foreground Color", 0xc0caf5),
        ("Cursor Color", 0xc0caf5),
        ("Selection Color", 0x283457),
        ("Selected Text Color", 0xc0caf5),
        ("Link Color", 0x73daca),
        ("Foreground Color (Light)", 0x000000),
    ] {
        text.push_str(&format!("<key>{key}</key>\n"));
        text.push_str(&iterm_colour_dict(colour, "sRGB"));
    }
    text.push_str("<key>Badge &amp; Tab</key><string>x</string>\n</dict>\n</plist>\n");
    let report = convert(ImportFormat::ITerm, &text, None).unwrap();
    assert_eq!(
        report.scheme,
        expected(
            "",
            0xc0caf5,
            0x1a1b26,
            Some(0xc0caf5),
            Some(0x283457),
            Some(0xc0caf5),
            PALETTE
        )
    );
    assert_eq!(
        report.dropped,
        ["Link Color", "Foreground Color (Light)", "Badge & Tab"]
    );
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    assert_round_trips(&report);
}

#[test]
fn iterm_dark_variants_stand_in_and_other_spaces_are_noted() {
    let text = format!(
        "<plist><dict><key>Background Color (Dark)</key>{}\
         <key>Foreground Color</key>{}</dict></plist>",
        iterm_colour_dict(0x101010, "P3"),
        iterm_colour_dict(0xeeeeee, "sRGB"),
    );
    let report = convert(ImportFormat::ITerm, &text, None).unwrap();
    assert_eq!(report.scheme.background, rgb(0x101010));
    assert!(report.notes.iter().any(|n| n.contains("(Dark)")));
    assert!(report.notes.iter().any(|n| n.contains("\"P3\"")));
}

// Colours as Terminal.app archives them (each an `NSKeyedArchiver` binary plist of
// one `NSColor`), copied from the profiles macOS ships in Terminal.app's Initial
// Settings folder.

/// Clear Dark's TextColor (calibrated RGB).
const CALIBRATED_TEXT: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGjCwwTVSRudWxs0w0ODxARElVOU1JHQlxOU0NvbG9yU3BhY2VWJGNsYXNz\
    TxAnMC44Nzc2MTA4MDk5IDAuODc3NjEwODA5OSAwLjg3NzYxMDgwOTkAEAGAAtIUFRYXWiRjbGFzc25hbWVY\
    JGNsYXNzZXNXTlNDb2xvcqIWGFhOU09iamVjdAgRGiQpMjdJTFFTV11kand+qKqssbzFzdAAAAAAAAABAQAA\
    AAAAAAAZAAAAAAAAAAAAAAAAAAAA2Q==";

/// Clear Dark's BackgroundColor (calibrated RGB, alpha 0.95).
const CALIBRATED_TRANSLUCENT_BACKGROUND: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGjCwwTVSRudWxs0w0ODxARElVOU1JHQlxOU0NvbG9yU3BhY2VWJGNsYXNz\
    TxAsMC4wOTgzODc3MTE0NSAwLjExMzg3Nzc0NzYgMC4xNTI2MDI4MzggMC45NQAQAYAC0hQVFhdaJGNsYXNz\
    bmFtZVgkY2xhc3Nlc1dOU0NvbG9yohYYWE5TT2JqZWN0CBEaJCkyN0lMUVNXXWRqd36tr7G2wcrS1QAAAAAA\
    AAEBAAAAAAAAABkAAAAAAAAAAAAAAAAAAADe";

/// Clear Dark's ANSIRedColor (device RGB).
const DEVICE_RED: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGjCwwTVSRudWxs0w0ODxARElVOU1JHQlxOU0NvbG9yU3BhY2VWJGNsYXNz\
    TxAnMC43MDUwOTAwODI5IDAuMzM1NzE1NzMwMSAwLjI4MTE0OTA2NDMAEAKAAtIUFRYXWiRjbGFzc25hbWVY\
    JGNsYXNzZXNXTlNDb2xvcqIWGFhOU09iamVjdAgRGiQpMjdJTFFTV11kand+qKqssbzFzdAAAAAAAAABAQAA\
    AAAAAAAZAAAAAAAAAAAAAAAAAAAA2Q==";

/// Clear Dark's ANSIBrightBlueColor (device RGB).
const DEVICE_BRIGHT_BLUE: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGjCwwTVSRudWxs0w0ODxARElVOU1JHQlxOU0NvbG9yU3BhY2VWJGNsYXNz\
    TxAnMC40MDM5MjE1Njg2IDAuNzA5ODAzOTIxNiAwLjkyOTQxMTc2NDcAEAKAAtIUFRYXWiRjbGFzc25hbWVY\
    JGNsYXNzZXNXTlNDb2xvcqIWGFhOU09iamVjdAgRGiQpMjdJTFFTV11kand+qKqssbzFzdAAAAAAAAABAQAA\
    AAAAAAAZAAAAAAAAAAAAAAAAAAAA2Q==";

/// Pro's CursorColor (calibrated white).
const CALIBRATED_GREY_CURSOR: &str = "\
    YnBsaXN0MDDUAQIDBAUGFRZYJHZlcnNpb25YJG9iamVjdHNZJGFyY2hpdmVyVCR0b3ASAAGGoKMHCA9VJG51\
    bGzTCQoLDA0OViRjbGFzc1xOU0NvbG9yU3BhY2VXTlNXaGl0ZYACEANLMC4zMDI0MTkzNgDSEBESE1okY2xh\
    c3NuYW1lWCRjbGFzc2VzV05TQ29sb3KiEhRYTlNPYmplY3RfEA9OU0tleWVkQXJjaGl2ZXLRFxhUcm9vdIAB\
    CBEaIy0yNztBSE9cZGZodHmEjZWYobO2uwAAAAAAAAEBAAAAAAAAABkAAAAAAAAAAAAAAAAAAAC9";

/// Homebrew's SelectionColor (calibrated RGB, alpha 0.65).
const CALIBRATED_TRANSLUCENT_SELECTION: &str = "\
    YnBsaXN0MDDUAQIDBAUGFRZYJHZlcnNpb25YJG9iamVjdHNZJGFyY2hpdmVyVCR0b3ASAAGGoKMHCA9VJG51\
    bGzTCQoLDA0OVU5TUkdCViRjbGFzc1xOU0NvbG9yU3BhY2VPEB4wLjAzNDU3ODM5NSAwIDAuOTEzMjY1MzEg\
    MC42NQCAAhAB0hAREhNaJGNsYXNzbmFtZVgkY2xhc3Nlc1dOU0NvbG9yohIUWE5TT2JqZWN0XxAPTlNLZXll\
    ZEFyY2hpdmVy0RcYVHJvb3SAAQgRGiMtMjc7QUhOVWKDhYeMl6Coq7TGyc4AAAAAAAABAQAAAAAAAAAZAAAA\
    AAAAAAAAAAAAAAAA0A==";

/// Pro's Font: a well-formed archive of an `NSFont`, not a colour.
const MONACO_FONT: &str = "\
    YnBsaXN0MDDUAQIDBAUGGBlYJHZlcnNpb25YJG9iamVjdHNZJGFyY2hpdmVyVCR0b3ASAAGGoKQHCBESVSRu\
    dWxs1AkKCwwNDg8QViRjbGFzc1hOU2ZGbGFnc1ZOU05hbWVWTlNTaXplgAMQEIACI0AkAAAAAAAAVk1vbmFj\
    b9ITFBUWWiRjbGFzc25hbWVYJGNsYXNzZXNWTlNGb250ohUXWE5TT2JqZWN0XxAPTlNLZXllZEFyY2hpdmVy\
    0RobVHJvb3SAAQgRGiMtMjc8QktSW2Jpa21veH+Ej5ifoqu9wMUAAAAAAAABAQAAAAAAAAAcAAAAAAAAAAAA\
    AAAAAAAAxw==";

// Colours as macOS 27 archives them (`NSKeyedArchiver.archivedData(withRootObject:
// requiringSecureCoding: false)` of each `NSColor`), each ICC profile cut to its header
// and description tag and the plist written again around it.

/// sRGB 0.2 0.4 0.6 (`#336699`): `NSComponents` in a custom space whose `NSID` is 7.
const SRGB_336699: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGmCwwZHyAlVSRudWxs1g0ODxAREhMUFRYXGFYkY2xhc3NcTlNDb21wb25l\
    bnRzVU5TUkdCXE5TQ29sb3JTcGFjZV8QEk5TQ3VzdG9tQ29sb3JTcGFjZV8QEE5TTGluZWFyRXhwb3N1cmWA\
    BU0wLjIgMC40IDAuNiAxTxAnMC4xNTU4MTYyNzE5IDAuMzE5ODYzNzM2NiAwLjUyODkzNDE4MDcAEAGAAkEx\
    0xobDRwdHlROU0lEVU5TSUNDEAeAA4AETxD8AAAA/Expbm8CEAAAbW50clJHQiBYWVogB84AAgAJAAYAMQAA\
    YWNzcE1TRlQAAAAASUVDIHNSR0IAAAAAAAAAAAAAAAAAAPbWAAEAAAAA0y1IUCAgAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABZGVzYwAAAJAAAABsZGVzYwAAAAAAAAASc1JHQiBJ\
    RUM2MTk2Ni0yLjEAAAAAAAAAAAAAABJzUkdCIElFQzYxOTY2LTIuMQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA0iEiECNaJGNsYXNzbmFtZVgkY2xhc3Nlc6IQJFhOU09iamVj\
    dNIhIiYnV05TQ29sb3KiJiQACAARABoAJAApADIANwBJAEwAUQBTAFoAYABtAHQAgQCHAJQAqQC8AL4AzAD2\
    APgA+gD8AQMBCAEOARABEgEUAhMCGAIjAiwCLwI4Aj0CRQAAAAAAAAIBAAAAAAAAACgAAAAAAAAAAAAAAAAA\
    AAJI";

/// Display P3 0.2 0.4 0.6: `NSComponents` in a custom space (`NSSpaceID` 12) whose ICC
/// profile is ICC v4, its description multi-localized Unicode.
const DISPLAY_P3_336699: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGmCwwZHyAlVSRudWxs1g0ODxAREhMUFRYXGFYkY2xhc3NcTlNDb21wb25l\
    bnRzVU5TUkdCXE5TQ29sb3JTcGFjZV8QEk5TQ3VzdG9tQ29sb3JTcGFjZV8QEE5TTGluZWFyRXhwb3N1cmWA\
    BU0wLjIgMC40IDAuNiAxTxAoMC4wOTEwODM3MTI4OCAwLjMyNTYxNTIyNzIgMC41NDcyNjgzOTA3ABABgAJB\
    MdMaDRscHR5VTlNJQ0NZTlNTcGFjZUlEgAOABBAMTxDAAAAAwGFwcGwEAAAAbW50clJHQiBYWVogB+YAAQAB\
    AAAAAAAAYWNzcEFQUEwAAAAAQVBQTAAAAAAAAAAAAAAAAAAAAAAAAPbWAAEAAAAA0y1hcHBsAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABZGVzYwAAAJAAAAAwbWx1YwAAAAAAAAAB\
    AAAADGVuVVMAAAAUAAAAHABEAGkAcwBwAGwAYQB5ACAAUAAz0iEiECNaJGNsYXNzbmFtZVgkY2xhc3Nlc6IQ\
    JFhOU09iamVjdNIhIiYnV05TQ29sb3KiJiQACAARABoAJAApADIANwBJAEwAUQBTAFoAYABtAHQAgQCHAJQA\
    qQC8AL4AzAD3APkA+wD9AQQBCgEUARYBGAEaAd0B4gHtAfYB+QICAgcCDwAAAAAAAAIBAAAAAAAAACgAAAAA\
    AAAAAAAAAAAAAAIS";

/// Gamma-2.2 grey 0.3 (`#4d4d4d`): `NSComponents` in a custom space (`NSID` 9) whose ICC
/// profile is ICC v2, its description ASCII text.
const GAMMA_22_GREY_4D: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGmCwwZISInVSRudWxs1g0ODxAREhMUFRYXGFdOU1doaXRlViRjbGFzc1xO\
    U0NvbXBvbmVudHNcTlNDb2xvclNwYWNlXxASTlNDdXN0b21Db2xvclNwYWNlXxAQTlNMaW5lYXJFeHBvc3Vy\
    ZU0wLjIzNDA0NTk4MjQAgAVFMC4zIDEQA4ACQTHUGhscDh0eHyBUTlNJRFVOU0lDQ1dOU01vZGVsEAmAAxAA\
    gARPEQEMAAABDGFwcGwCAAAAbW50ckdSQVlYWVogB9wACAAXAA8ALgAPYWNzcEFQUEwAAAAAbm9uZQAAAAAA\
    AAAAAAAAAAAAAAAAAPbWAAEAAAAA0y1hcHBsAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAABZGVzYwAAAJAAAAB5ZGVzYwAAAAAAAAAfR2VuZXJpYyBHcmF5IEdhbW1hIDIuMiBQcm9m\
    aWxlAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAANIjJBAlWiRjbGFzc25hbWVYJGNsYXNzZXOiECZYTlNPYmplY3TSIyQo\
    KVdOU0NvbG9yoigmAAgAEQAaACQAKQAyADcASQBMAFEAUwBaAGAAbQB1AHwAiQCWAKsAvgDMAM4A1ADWANgA\
    2gDjAOgA7gD2APgA+gD8AP4CDgITAh4CJwIqAjMCOAJAAAAAAAAAAgEAAAAAAAAAKgAAAAAAAAAAAAAAAAAA\
    AkM=";

/// `NSColor.textColor`: a named colour (`NSColorSpace` 6) whose fallback is gamma-2.2
/// grey 0.
const NAMED_TEXT_COLOR: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGpCwwXGBkjKywxVSRudWxs1Q0ODxAREhMUFRZWJGNsYXNzW05TQ29sb3JO\
    YW1lXE5TQ29sb3JTcGFjZV1OU0NhdGFsb2dOYW1lV05TQ29sb3KACIADEAaAAoAEVlN5c3RlbVl0ZXh0Q29s\
    b3LWGg0bDxwdHhIfICEiV05TV2hpdGVcTlNDb21wb25lbnRzXxASTlNDdXN0b21Db2xvclNwYWNlXxAQTlNM\
    aW5lYXJFeHBvc3VyZUIwAEMwIDEQA4AFQTHUJCUmDScoKSpUTlNJRFVOU0lDQ1dOU01vZGVsEAmABhAAgAdP\
    EQEMAAABDGFwcGwCAAAAbW50ckdSQVlYWVogB9wACAAXAA8ALgAPYWNzcEFQUEwAAAAAbm9uZQAAAAAAAAAA\
    AAAAAAAAAAAAAPbWAAEAAAAA0y1hcHBsAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAABZGVzYwAAAJAAAAB5ZGVzYwAAAAAAAAAfR2VuZXJpYyBHcmF5IEdhbW1hIDIuMiBQcm9maWxl\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAANItLg8vWiRjbGFzc25hbWVYJGNsYXNzZXOiDzBYTlNPYmplY3TSLS4RMqIR\
    MAAIABEAGgAkACkAMgA3AEkATABRAFMAXQBjAG4AdQCBAI4AnACkAKYAqACqAKwArgC1AL8AzADUAOEA9gEJ\
    AQwBEAESARQBFgEfASQBKgEyATQBNgE4AToCSgJPAloCYwJmAm8CdAAAAAAAAAIBAAAAAAAAADMAAAAAAAAA\
    AAAAAAAAAAJ3";

/// `NSColor.systemRed`: a named colour whose fallback is sRGB 1 0 0.
const NAMED_SYSTEM_RED: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGpCwwXGBkjKSovVSRudWxs1Q0ODxAREhMUFRZWJGNsYXNzW05TQ29sb3JO\
    YW1lXE5TQ29sb3JTcGFjZV1OU0NhdGFsb2dOYW1lV05TQ29sb3KACIADEAaAAoAEVlN5c3RlbV5zeXN0ZW1S\
    ZWRDb2xvctYNGhsPHB0SHh8gISJcTlNDb21wb25lbnRzVU5TUkdCXxASTlNDdXN0b21Db2xvclNwYWNlXxAQ\
    TlNMaW5lYXJFeHBvc3VyZUcxIDAgMCAxTxAdMC45ODU5NTQxNjU1IDAgMC4wMjY5NDAwMDg2MwAQAYAFQTHT\
    JCUNJicoVE5TSURVTlNJQ0MQB4AGgAdPEPwAAAD8TGlubwIQAABtbnRyUkdCIFhZWiAHzgACAAkABgAxAABh\
    Y3NwTVNGVAAAAABJRUMgc1JHQgAAAAAAAAAAAAAAAAAA9tYAAQAAAADTLUhQICAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAFkZXNjAAAAkAAAAGxkZXNjAAAAAAAAABJzUkdCIElF\
    QzYxOTY2LTIuMQAAAAAAAAAAAAAAEnNSR0IgSUVDNjE5NjYtMi4xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADSKywPLVokY2xhc3NuYW1lWCRjbGFzc2Vzog8uWE5TT2JqZWN0\
    0issETCiES4ACAARABoAJAApADIANwBJAEwAUQBTAF0AYwBuAHUAgQCOAJwApACmAKgAqgCsAK4AtQDEANEA\
    3gDkAPkBDAEUATQBNgE4AToBQQFGAUwBTgFQAVICUQJWAmECagJtAnYCewAAAAAAAAIBAAAAAAAAADEAAAAA\
    AAAAAAAAAAAAAAJ+";

/// Extended sRGB 1.2 -0.1 0.6, outside sRGB: `NSComponents` in a custom space whose
/// `NSSpaceID` is 14 and whose ICC profile is sRGB's.
const EXTENDED_SRGB_WIDE: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGmCwwZHyAlVSRudWxs1g0ODxAREhMUFRYXGFYkY2xhc3NcTlNDb21wb25l\
    bnRzVU5TUkdCXE5TQ29sb3JTcGFjZV8QEk5TQ3VzdG9tQ29sb3JTcGFjZV8QEE5TTGluZWFyRXhwb3N1cmWA\
    BU4xLjIgLTAuMSAwLjYgMU8QETEgMCAwLjUzMTAzNTQ4MjkAEAGAAkEw0xoNGxwdHlVOU0lDQ1lOU1NwYWNl\
    SUSAA4AEEA5PEPwAAAD8TGlubwIQAABtbnRyUkdCIFhZWiAHzgACAAkABgAxAABhY3NwTVNGVAAAAABJRUMg\
    c1JHQgAAAAAAAAAAAAAAAAAA9tYAAQAAAADTLUhQICAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAFkZXNjAAAAkAAAAGxkZXNjAAAAAAAAABJzUkdCIElFQzYxOTY2LTIuMQAAAAAA\
    AAAAAAAAEnNSR0IgSUVDNjE5NjYtMi4xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAADSISIQI1okY2xhc3NuYW1lWCRjbGFzc2VzohAkWE5TT2JqZWN00iEiJidXTlNDb2xvcqIm\
    JAAIABEAGgAkACkAMgA3AEkATABRAFMAWgBgAG0AdACBAIcAlACpALwAvgDNAOEA4wDlAOcA7gD0AP4BAAEC\
    AQQCAwIIAhMCHAIfAigCLQI1AAAAAAAAAgEAAAAAAAAAKAAAAAAAAAAAAAAAAAAAAjg=";

/// White converted to extended sRGB (`NSColor(white: 1).usingColorSpace(.extendedSRGB)`):
/// `NSComponents` 1.000000119 for each of red, green and blue, past 1 only by float
/// rounding, in a custom space whose `NSSpaceID` is 14 and whose ICC profile is sRGB's.
const EXTENDED_SRGB_WHITE: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGmCwwZHyAlVSRudWxs1g0ODxAREhMUFRYXGFYkY2xhc3NcTlNDb21wb25l\
    bnRzVU5TUkdCXE5TQ29sb3JTcGFjZV8QEk5TQ3VzdG9tQ29sb3JTcGFjZV8QEE5TTGluZWFyRXhwb3N1cmWA\
    BU8QJTEuMDAwMDAwMTE5IDEuMDAwMDAwMTE5IDEuMDAwMDAwMTE5IDFGMSAxIDEAEAGAAkEx0xoNGxwdHlVO\
    U0lDQ1lOU1NwYWNlSUSAA4AEEA5PEPwAAAD8TGlubwIQAABtbnRyUkdCIFhZWiAHzgACAAkABgAxAABhY3Nw\
    TVNGVAAAAABJRUMgc1JHQgAAAAAAAAAAAAAAAAAA9tYAAQAAAADTLUhQICAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAFkZXNjAAAAkAAAAGxkZXNjAAAAAAAAABJzUkdCIElFQzYx\
    OTY2LTIuMQAAAAAAAAAAAAAAEnNSR0IgSUVDNjE5NjYtMi4xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    AAAAAAAAAAAAAAAAAAAAAAAAAAAAAADSISIQI1okY2xhc3NuYW1lWCRjbGFzc2VzohAkWE5TT2JqZWN00iEi\
    JidXTlNDb2xvcqImJAAIABEAGgAkACkAMgA3AEkATABRAFMAWgBgAG0AdACBAIcAlACpALwAvgDmAO0A7wDx\
    APMA+gEAAQoBDAEOARACDwIUAh8CKAIrAjQCOQJBAAAAAAAAAgEAAAAAAAAAKAAAAAAAAAAAAAAAAAAAAkQ=";

/// A device CMYK colour (`NSColorSpace` 5), which aterm does not read.
const DEVICE_CMYK: &str = "\
    YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8QD05TS2V5\
    ZWRBcmNoaXZlctEICVRyb290gAGjCwwTVSRudWxs0w0ODxARElYkY2xhc3NcTlNDb2xvclNwYWNlVk5TQ01Z\
    S4ACEAVPEBAwLjEgMC4yIDAuMyAwLjEA0hQVFhdaJGNsYXNzbmFtZVgkY2xhc3Nlc1dOU0NvbG9yohYYWE5T\
    T2JqZWN0CBEaJCkyN0lMUVNXXWRreH+Bg5abpq+3ugAAAAAAAAEBAAAAAAAAABkAAAAAAAAAAAAAAAAAAADD";

/// A Terminal.app profile as Terminal.app exports one: `entries` are its archived
/// colours (key, base64), then the settings every profile carries.
fn terminal_app_profile(entries: &[(&str, &str)]) -> String {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    for (key, archive) in entries {
        text.push_str(&format!("\t<key>{key}</key>\n\t<data>\n"));
        for chunk in archive.as_bytes().chunks(68) {
            text.push('\t');
            text.push_str(core::str::from_utf8(chunk).unwrap());
            text.push('\n');
        }
        text.push_str("\t</data>\n");
    }
    text.push_str(
        "\t<key>FontAntialias</key>\n\t<true/>\n\
         \t<key>ProfileCurrentVersion</key>\n\t<real>2.0699999999999998</real>\n\
         \t<key>columnCount</key>\n\t<integer>100</integer>\n\
         \t<key>name</key>\n\t<string>Clear Dark</string>\n\
         \t<key>type</key>\n\t<string>Window Settings</string>\n</dict>\n</plist>\n",
    );
    text
}

#[test]
fn terminal_app_profile_converts_its_archived_colours() {
    let text = terminal_app_profile(&[
        ("ANSIBrightBlueColor", DEVICE_BRIGHT_BLUE),
        ("ANSIRedColor", DEVICE_RED),
        ("BackgroundColor", CALIBRATED_TRANSLUCENT_BACKGROUND),
        ("CursorColor", CALIBRATED_GREY_CURSOR),
        ("Font", "YnBsaXN0MDA="),
        ("SelectionColor", CALIBRATED_TRANSLUCENT_SELECTION),
        ("TextBoldColor", CALIBRATED_TEXT),
        ("TextColor", CALIBRATED_TEXT),
    ]);
    for name in [Some("Clear Dark.terminal"), Some("Clear Dark.plist"), None] {
        let report = import_theme(name, text.as_bytes(), None).unwrap();
        assert_eq!(report.format.label(), "Terminal.app", "{name:?}");
        let mut ansi = ColorScheme::default().ansi;
        ansi[1] = rgb(0xb45648);
        ansi[12] = rgb(0x67b5ed);
        assert_eq!(
            report.scheme,
            ColorScheme {
                ansi,
                ..expected(
                    "Clear Dark",
                    0xe0e0e0,
                    0x191d27,
                    Some(0x4d4d4d),
                    Some(0x0900e9),
                    None,
                    [0; 16]
                )
            }
        );
        assert_eq!(
            report.dropped,
            [
                "Font",
                "TextBoldColor",
                "FontAntialias",
                "ProfileCurrentVersion",
                "columnCount",
                "type"
            ]
        );
        for space in ["device RGB", "calibrated RGB", "calibrated white"] {
            let note = format!(
                "colours in the \"{space}\" colour space were read as sRGB and may look \
                 slightly different"
            );
            assert!(report.notes.contains(&note), "{:?}", report.notes);
        }
        assert_round_trips(&report);
    }
}

#[test]
fn terminal_app_archives_that_do_not_hold_a_colour_are_dropped_not_fatal() {
    let foreground = ("TextColor", CALIBRATED_TEXT);
    let background = ("BackgroundColor", CALIBRATED_TRANSLUCENT_BACKGROUND);
    let mut damaged = Vec::new();
    for (index, original) in DEVICE_RED.char_indices() {
        damaged.push(DEVICE_RED[..index].to_string());
        for other in ['A', '/', 'z'] {
            if other != original {
                let mut text = DEVICE_RED.to_string();
                text.replace_range(index..index + 1, &other.to_string());
                damaged.push(text);
            }
        }
    }
    damaged.push("not base64 at all".to_string());
    damaged.push(MONACO_FONT.to_string());
    for archive in &damaged {
        let text = terminal_app_profile(&[("ANSIRedColor", archive), foreground, background]);
        let report = import_theme(Some("x.terminal"), text.as_bytes(), None)
            .unwrap_or_else(|e| panic!("{archive}: {e}"));
        assert_eq!(report.scheme.foreground, rgb(0xe0e0e0), "{archive}");
        if report.dropped.iter().any(|key| key == "ANSIRedColor") {
            assert!(
                report.notes.iter().any(|n| n.contains("\"ANSIRedColor\"")),
                "{archive}: {:?}",
                report.notes
            );
        }
    }
}

#[test]
fn terminal_app_text_or_background_it_cannot_read_is_named_in_the_error() {
    let foreground = ("TextColor", CALIBRATED_TEXT);
    let background = ("BackgroundColor", CALIBRATED_TRANSLUCENT_BACKGROUND);
    let import = |entries: &[(&str, &str)]| {
        let text = terminal_app_profile(entries);
        import_theme(Some("x.terminal"), text.as_bytes(), None).map_err(|e| e.to_string())
    };
    let unreadable = |key: &str| {
        Err(format!(
            "not a readable Terminal.app theme: \"{key}\" is not a colour aterm can read"
        ))
    };
    assert_eq!(
        import(&[("TextColor", "YnBsaXN0MDA="), background]),
        unreadable("TextColor")
    );
    assert_eq!(
        import(&[foreground, ("BackgroundColor", DEVICE_CMYK)]),
        unreadable("BackgroundColor")
    );
    assert_eq!(
        import(&[background]),
        Err("the Terminal.app theme sets no foreground colour".to_string())
    );
}

/// `bytes` as base64, the inverse of [`base64_decode`].
fn base64_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (i, &byte)| {
            word | u32::from(byte) << (16 - 8 * i)
        });
        for i in 0..=chunk.len() {
            out.push(char::from(DIGITS[(word >> (18 - 6 * i) & 63) as usize]));
        }
    }
    while !out.len().is_multiple_of(4) {
        out.push('=');
    }
    out
}

#[test]
fn terminal_app_colours_read_their_own_components_and_name_their_space() {
    let text = terminal_app_profile(&[
        ("ANSIBlackColor", NAMED_TEXT_COLOR),
        ("ANSIBlueColor", DISPLAY_P3_336699),
        ("ANSIGreenColor", DEVICE_CMYK),
        ("ANSIRedColor", NAMED_SYSTEM_RED),
        ("BackgroundColor", GAMMA_22_GREY_4D),
        ("TextColor", SRGB_336699),
    ]);
    let report = import_theme(Some("x.terminal"), text.as_bytes(), None).unwrap();
    let mut ansi = ColorScheme::default().ansi;
    ansi[0] = rgb(0x000000);
    ansi[1] = rgb(0xff0000);
    ansi[4] = rgb(0x336699);
    assert_eq!(
        report.scheme,
        ColorScheme {
            ansi,
            ..expected("Clear Dark", 0x336699, 0x4d4d4d, None, None, None, [0; 16])
        }
    );
    let space = |name: &str| {
        format!(
            "colours in the \"{name}\" colour space were read as sRGB and may look slightly \
             different"
        )
    };
    let named = |key: &str, name: &str| {
        format!(
            "\"{key}\" is the named colour \"{name}\": Terminal.app shows the system's \
             current colour for it, which can differ (in Dark Mode, say); the fallback saved \
             with it was used"
        )
    };
    assert_eq!(
        report.notes[..5],
        [
            named("ANSIBlackColor", "textColor"),
            space("Generic Gray Gamma 2.2 Profile"),
            space("Display P3"),
            "\"ANSIGreenColor\" is not an archived RGB or grey colour, so it was dropped"
                .to_string(),
            named("ANSIRedColor", "systemRedColor"),
        ]
    );
    assert!(
        report.notes[5].starts_with("ANSI colours "),
        "{:?}",
        report.notes
    );
    assert_eq!(report.notes.len(), 6, "{:?}", report.notes);
    assert_round_trips(&report);

    let text = terminal_app_profile(&[
        ("BackgroundColor", NAMED_SYSTEM_RED),
        ("TextColor", SRGB_336699),
    ]);
    let report = import_theme(Some("x.terminal"), text.as_bytes(), None).unwrap();
    assert_eq!(report.scheme.background, rgb(0xff0000));
    assert!(
        !report.notes.iter().any(|n| n.contains("colour space")),
        "sRGB is aterm's own space: {:?}",
        report.notes
    );

    let mut unsigned_icc = base64_decode(DISPLAY_P3_336699).unwrap();
    let signature = unsigned_icc.windows(4).position(|w| w == b"acsp").unwrap();
    unsigned_icc[signature] = b'x';
    let mut unnamed_red = base64_decode(NAMED_SYSTEM_RED).unwrap();
    let name = unnamed_red
        .windows(15)
        .position(|w| w == b"\x5esystemRedColor")
        .unwrap();
    unnamed_red[name] = 0x4e;
    let text = terminal_app_profile(&[
        ("BackgroundColor", &base64_encode(&unnamed_red)),
        ("TextColor", &base64_encode(&unsigned_icc)),
    ]);
    let report = import_theme(Some("x.terminal"), text.as_bytes(), None).unwrap();
    assert_eq!(report.scheme.foreground, rgb(0x336699));
    assert_eq!(report.scheme.background, rgb(0xff0000));
    assert_eq!(
        report.notes[..2],
        [
            "\"BackgroundColor\" is a named colour: Terminal.app shows the system's current \
             colour for it, which can differ (in Dark Mode, say); the fallback saved with it \
             was used",
            "colours in a colour space aterm could not name were read as sRGB and may look \
             slightly different",
        ]
    );
}

#[test]
fn terminal_app_extended_srgb_is_srgb_and_a_colour_outside_it_is_noted_as_clipped() {
    let text = terminal_app_profile(&[
        ("ANSIRedColor", EXTENDED_SRGB_WIDE),
        ("BackgroundColor", SRGB_336699),
        ("TextColor", SRGB_336699),
    ]);
    let report = import_theme(Some("x.terminal"), text.as_bytes(), None).unwrap();
    assert_eq!(report.scheme.ansi[1], rgb(0xff0099));
    assert_eq!(
        report.notes[0],
        "\"ANSIRedColor\" lies outside sRGB (a component is below 0 or above 1), so it was \
         clipped to fit"
    );
    assert!(
        report.notes[1].starts_with("ANSI colours "),
        "{:?}",
        report.notes
    );
    assert_eq!(
        report.notes.len(),
        2,
        "extended sRGB is sRGB from 0 to 1, so no colour space is noted: {:?}",
        report.notes
    );
    assert_round_trips(&report);
}

#[test]
fn terminal_app_extended_srgb_past_1_by_float_rounding_is_not_noted_as_clipped() {
    let text = terminal_app_profile(&[
        ("BackgroundColor", EXTENDED_SRGB_WHITE),
        ("TextColor", SRGB_336699),
    ]);
    let report = import_theme(Some("x.terminal"), text.as_bytes(), None).unwrap();
    assert_eq!(report.scheme.background, rgb(0xffffff));
    assert!(
        !report
            .notes
            .iter()
            .any(|n| n.contains("clipped") || n.contains("colour space")),
        "1.000000119 rounds to the same byte as 1, and extended sRGB is sRGB: {:?}",
        report.notes
    );
    assert_round_trips(&report);
}

#[test]
fn icc_descriptions_read_as_text_or_unicode_and_damage_reads_as_none() {
    let profile = |tag: &[u8]| {
        let mut icc = vec![0u8; 128];
        icc[36..40].copy_from_slice(b"acsp");
        icc.extend(1u32.to_be_bytes());
        icc.extend(b"desc");
        icc.extend(144u32.to_be_bytes());
        icc.extend((tag.len() as u32).to_be_bytes());
        icc.extend(tag);
        icc
    };
    let mut text = b"desc\0\0\0\0".to_vec();
    text.extend(5u32.to_be_bytes());
    text.extend(b"Gray\0");
    assert_eq!(icc_description(&profile(&text)).as_deref(), Some("Gray"));
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_be_bytes).collect() };
    let mut unicode = b"mluc\0\0\0\0".to_vec();
    unicode.extend(2u32.to_be_bytes());
    unicode.extend(12u32.to_be_bytes());
    for (language, at, name) in [(b"frFR", 40u32, "Gris"), (b"enUS", 48, "Grey")] {
        unicode.extend(language);
        unicode.extend((utf16(name).len() as u32).to_be_bytes());
        unicode.extend(at.to_be_bytes());
    }
    unicode.extend(utf16("Gris"));
    unicode.extend(utf16("Grey"));
    assert_eq!(icc_description(&profile(&unicode)).as_deref(), Some("Grey"));
    let whole = profile(&unicode);
    for len in 0..whole.len() {
        assert_eq!(icc_description(&whole[..len]), None, "{len}");
    }
    let mut unsigned = profile(&text);
    unsigned[36] = b'x';
    assert_eq!(icc_description(&unsigned), None);
}

#[test]
fn wezterm_scheme_converts_structurally() {
    let quoted = |colours: &[u32]| -> Vec<String> {
        colours.iter().map(|c| format!("\"#{c:06x}\"")).collect()
    };
    let text = format!(
        "# Tokyo Night\n\
         [colors]\n\
         ansi = [\n    {},\n]\n\
         background = \"#1a1b26\"\n\
         brights = [{}] # one line\n\
         compose_cursor = \"#ff9e64\"\n\
         cursor_bg = \"#c0caf5\"\n\
         cursor_border = \"#c0caf5\"\n\
         cursor_fg = \"#1a1b26\"\n\
         foreground = \"#c0caf5\"\n\
         selection_bg = \"#283457\"\n\
         selection_fg = \"#c0caf5\"\n\
         split = \"#3b4261\"\n\
         \n\
         [colors.indexed]\n\
         16 = \"#ff9e64\"\n\
         \n\
         [colors.tab_bar.active_tab]\n\
         bg_color = \"#7aa2f7\"\n\
         fg_color = \"#16161e\"\n\
         \n\
         [metadata]\n\
         aliases = []\n\
         author = \"enkia\"\n\
         name = \"Tokyo Night\"\n\
         origin_url = \"https://github.com/folke/tokyonight.nvim\"\n",
        quoted(&PALETTE[..8]).join(",\n    "),
        quoted(&PALETTE[8..]).join(", "),
    );
    for name in [Some("tokyonight.toml"), None] {
        let report = import_theme(name, text.as_bytes(), None).unwrap();
        assert_eq!(report.format.label(), "WezTerm", "{name:?}");
        let mut want = expected(
            "Tokyo Night",
            0xc0caf5,
            0x1a1b26,
            Some(0xc0caf5),
            Some(0x283457),
            Some(0xc0caf5),
            PALETTE,
        );
        want.chrome.border = Some(rgb(0x3b4261));
        want.chrome.active_tab = Some(rgb(0x7aa2f7));
        assert_eq!(report.scheme, want);
        assert_eq!(report.author.as_deref(), Some("enkia"));
        assert_eq!(
            report.dropped,
            [
                "colors.compose_cursor",
                "colors.cursor_border",
                "colors.cursor_fg",
                "colors.indexed.16",
                "colors.tab_bar.active_tab.fg_color",
                "metadata.aliases",
                "metadata.origin_url"
            ]
        );
        assert!(report.notes.iter().any(|n| n.contains("above 15")));
        assert_round_trips(&report);
    }
}

#[test]
fn toml_inline_tables_expand_at_every_depth() {
    let wezterm = "[colors]\nforeground = \"#c0caf5\"\nbackground = \"#1a1b26\"\n\
                   tab_bar = { active_tab = { bg_color = \"#123456\", fg_color = \"#000\" } }\n";
    let report = convert(ImportFormat::WezTerm, wezterm, None).unwrap();
    assert_eq!(report.scheme.chrome.active_tab, Some(rgb(0x123456)));
    assert_eq!(report.dropped, ["colors.tab_bar.active_tab.fg_color"]);
    let alacritty = "colors = { primary = { foreground = '#ffffff', background = '#000000' } }\n";
    let report = convert(ImportFormat::Alacritty, alacritty, None).unwrap();
    assert_eq!(
        (report.scheme.foreground, report.scheme.background),
        (rgb(0xffffff), rgb(0x000000))
    );
}

#[test]
fn terminal_app_and_wezterm_files_are_told_from_iterm2_and_alacritty() {
    let terminal = "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n\
                    <key>name</key><string>Basic</string>\n\
                    <key>type</key><string>Window Settings</string>\n</dict>\n</plist>\n";
    let iterm = "<?xml version=\"1.0\"?>\n<plist><dict><key>Ansi 0 Color</key><dict/>\
                 <key>Background Color</key><dict/></dict></plist>\n";
    let wezterm = "[colors]\nforeground = \"#c0caf5\"\nbackground = \"#1a1b26\"\n";
    let wezterm_dotted = "colors.ansi = [\"#000000\"]\n";
    let wezterm_metadata = "[metadata]\nname = \"Night\"\n";
    let alacritty = "[colors.primary]\nforeground = '#ffffff'\n";
    let alacritty_inline = "[colors]\nprimary = { foreground = '#ffffff' }\n";
    for (name, text, label) in [
        (Some("Basic.terminal"), "", "Terminal.app"),
        (Some("Basic.plist"), terminal, "Terminal.app"),
        (None, terminal, "Terminal.app"),
        (Some("x.plist"), iterm, "iTerm2"),
        (None, iterm, "iTerm2"),
        (Some("x.toml"), wezterm, "WezTerm"),
        (None, wezterm, "WezTerm"),
        (None, wezterm_dotted, "WezTerm"),
        (None, wezterm_metadata, "WezTerm"),
        (Some("x.toml"), alacritty, "Alacritty"),
        (None, alacritty, "Alacritty"),
        (None, alacritty_inline, "Alacritty"),
    ] {
        assert_eq!(
            detect_format(name, text).map(ImportFormat::label),
            Some(label),
            "{name:?} {text:?}"
        );
    }
    for (text, named, not) in [
        (terminal, "Terminal.app", "iTerm2"),
        (iterm, "iTerm2", "Terminal.app"),
        (wezterm, "WezTerm", "Alacritty"),
        (alacritty, "Alacritty", "WezTerm"),
    ] {
        let label = sniff_foreign_format(text).map_or("", ForeignFormat::label);
        assert!(
            label.contains(named) && !label.contains(not),
            "{text:?}: {label}"
        );
    }
}

fn windows_terminal_scheme(name: &str) -> String {
    let names = [
        "black", "red", "green", "yellow", "blue", "purple", "cyan", "white",
    ];
    let mut text = format!(
        "{{\n  \"name\": \"{name}\",\n  \"foreground\": \"#C0CAF5\",\n  \
         \"background\": \"#1A1B26\",\n  \"cursorColor\": \"#c0caf5\",\n  \
         \"selectionBackground\": \"#283457\",\n"
    );
    for (index, colour_name) in names.iter().enumerate() {
        text.push_str(&format!(
            "  \"{colour_name}\": \"#{:06x}\",\n",
            PALETTE[index]
        ));
        let mut bright = String::from("bright");
        bright.push_str(&colour_name[..1].to_uppercase());
        bright.push_str(&colour_name[1..]);
        text.push_str(&format!(
            "  \"{bright}\": \"#{:06x}\",\n",
            PALETTE[index + 8]
        ));
    }
    text.push_str("  \"opacity\": 90\n}");
    text
}

#[test]
fn windows_terminal_converts_structurally() {
    let report = convert(
        ImportFormat::WindowsTerminal,
        &windows_terminal_scheme("Tokyo Night"),
        None,
    )
    .unwrap();
    assert_eq!(
        report.scheme,
        expected(
            "Tokyo Night",
            0xc0caf5,
            0x1a1b26,
            Some(0xc0caf5),
            Some(0x283457),
            None,
            PALETTE
        )
    );
    assert_eq!(report.dropped, ["opacity"]);
    assert_round_trips(&report);
}

#[test]
fn windows_terminal_settings_pick_a_scheme_by_name() {
    let text = format!(
        "// settings.json\n{{\n  \"profiles\": {{ \"list\": [] }},\n  /* schemes */\n  \
         \"schemes\": [\n{},\n{},\n],\n}}\n",
        windows_terminal_scheme("First"),
        windows_terminal_scheme("Second \\u00e9"),
    );
    let first = convert(ImportFormat::WindowsTerminal, &text, None).unwrap();
    assert_eq!(first.scheme.name, "First");
    assert!(first.notes.iter().any(|n| n.contains("2 schemes")));
    let second = convert(ImportFormat::WindowsTerminal, &text, Some("second é")).unwrap();
    assert_eq!(second.scheme.name, "Second é");
    let unpaired = windows_terminal_scheme("Pair \\uD800\\u0041 \\uD800\\uD83D\\uDE00");
    let unpaired = convert(ImportFormat::WindowsTerminal, &unpaired, None).unwrap();
    assert_eq!(unpaired.scheme.name, "Pair \u{fffd}A \u{fffd}😀");
    assert_eq!(
        decode_entities("&#x41;&#65;&#x+41;&#+65;"),
        "AA&#x+41;&#+65;"
    );
}

#[test]
fn base16_converts_structurally() {
    let text = "scheme: \"Ocean\"\nauthor: \"Chris Kempson (http://chriskempson.com)\"\n\
                base00: \"2b303b\"\nbase01: \"343d46\"\nbase02: \"4f5b66\"\n\
                base03: \"65737e\"\nbase04: \"a7adba\"\nbase05: \"c0c5ce\"\n\
                base06: \"dfe1e8\"\nbase07: \"eff1f5\"\nbase08: \"bf616a\"\n\
                base09: \"d08770\"\nbase0A: \"ebcb8b\"\nbase0B: \"a3be8c\"\n\
                base0C: \"96b5b4\"\nbase0D: \"8fa1b3\"\nbase0E: \"b48ead\"\n\
                base0F: \"ab7967\" # brown\nextra: 1\n";
    let report = convert(ImportFormat::Base16, text, None).unwrap();
    let ansi = [
        0x2b303b, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x8fa1b3, 0xb48ead, 0x96b5b4, 0xc0c5ce, 0x65737e,
        0xbf616a, 0xa3be8c, 0xebcb8b, 0x8fa1b3, 0xb48ead, 0x96b5b4, 0xeff1f5,
    ];
    assert_eq!(
        report.scheme,
        expected(
            "Ocean",
            0xc0c5ce,
            0x2b303b,
            Some(0xc0c5ce),
            Some(0x4f5b66),
            None,
            ansi
        )
    );
    assert_eq!(
        report.author.as_deref(),
        Some("Chris Kempson (http://chriskempson.com)")
    );
    assert_eq!(report.dropped, ["extra"]);
    assert!(!report.appearance_declared);
    assert_round_trips(&report);
}

#[test]
fn tinted_base24_takes_its_brights_and_variant() {
    let mut text =
        String::from("system: \"base24\"\nname: \"Paper\"\nvariant: \"light\"\npalette:\n");
    for (index, name) in BASE_NAMES.iter().enumerate() {
        text.push_str(&format!(
            "  {name}: \"#{:06x}\"\n",
            0x101010 * (index as u32 % 15 + 1)
        ));
    }
    let report = convert(ImportFormat::Base16, &text, None).unwrap();
    let base = |i: u32| rgb(0x101010 * (i % 15 + 1));
    assert_eq!(report.scheme.appearance, Appearance::Light);
    assert!(report.appearance_declared);
    assert_eq!(report.scheme.ansi[9], base(0x12));
    assert_eq!(report.scheme.ansi[14], base(0x15));
    assert_eq!(report.scheme.ansi[1], base(0x08));
    assert!(report.dropped.is_empty(), "{:?}", report.dropped);
    assert_round_trips(&report);
}

#[test]
fn unknown_keys_are_reported_not_fatal() {
    let cases: [(ImportFormat, &str, &str); 6] = [
        (
            ImportFormat::Kitty,
            "foreground #fff\nbackground #000\n",
            "frobnicate 1\n",
        ),
        (
            ImportFormat::Ghostty,
            "foreground = fff\nbackground = 000\n",
            "frobnicate = 1\n",
        ),
        (
            ImportFormat::Alacritty,
            "[colors.primary]\nforeground = '#fff'\nbackground = '#000'\n",
            "frobnicate = 1\n",
        ),
        (
            ImportFormat::WindowsTerminal,
            "{\"foreground\": \"#fff\", \"background\": \"#000\"",
            ", \"frobnicate\": 1",
        ),
        (
            ImportFormat::ITerm,
            "<plist><dict><key>Foreground Color</key><dict><key>Red Component</key><real>1</real><key>Green Component</key><real>1</real><key>Blue Component</key><real>1</real></dict><key>Background Color</key><dict><key>Red Component</key><real>0</real><key>Green Component</key><real>0</real><key>Blue Component</key><real>0</real></dict>",
            "<key>frobnicate</key><integer>1</integer>",
        ),
        (
            ImportFormat::Base16,
            "",
            "base+0: \"#112233\"\nfrobnicate: 1\n",
        ),
    ];
    let base16_body: String = BASE_NAMES
        .iter()
        .take(16)
        .map(|name| format!("{name}: \"808080\"\n"))
        .collect();
    for (format, body, extra) in cases {
        let close = match format {
            ImportFormat::WindowsTerminal => "}",
            ImportFormat::ITerm => "</dict></plist>",
            _ => "",
        };
        let body = if format == ImportFormat::Base16 {
            base16_body.as_str()
        } else {
            body
        };
        let clean = convert(format, &format!("{body}{close}"), None).unwrap();
        assert!(clean.dropped.is_empty(), "{format:?}: {:?}", clean.dropped);
        let with = convert(format, &format!("{body}{extra}{close}"), None).unwrap();
        assert!(
            with.dropped.iter().any(|k| k.ends_with("frobnicate")),
            "{format:?}: {:?}",
            with.dropped
        );
        assert_eq!(with.scheme, clean.scheme, "{format:?}");
    }
}

#[test]
fn oversize_input_is_refused_before_it_is_read() {
    let mut bytes = kitty_sample().into_bytes();
    bytes.resize(MAX_IMPORT_BYTES + 1, b'\n');
    assert_eq!(
        import_theme(Some("big.conf"), &bytes, None),
        Err(ImportError::TooLarge {
            bytes: MAX_IMPORT_BYTES + 1
        })
    );
    bytes.truncate(MAX_IMPORT_BYTES);
    assert!(import_theme(Some("big.conf"), &bytes, None).is_ok());
    let text = "x".repeat(MAX_IMPORT_BYTES + 1);
    assert!(matches!(
        convert(ImportFormat::Kitty, &text, None),
        Err(ImportError::TooLarge { .. })
    ));
}

#[test]
fn detection_follows_the_extension_then_the_content() {
    let kitty = kitty_sample();
    let ghostty = "background = #1a1b26\nforeground = #c0caf5\npalette = 0=#000000\n";
    let native = "foreground = #c0caf5\nbackground = #1a1b26\n";
    let alacritty = "[colors.primary]\nforeground = '#ffffff'\n";
    for (name, text, format) in [
        (Some("x.conf"), kitty.as_str(), ImportFormat::Kitty),
        (None, ghostty, ImportFormat::Ghostty),
        (Some("x.conf"), native, ImportFormat::Native),
        (Some("x"), alacritty, ImportFormat::Alacritty),
        (Some("x.toml"), native, ImportFormat::Alacritty),
        (Some("x.itermcolors"), "", ImportFormat::ITerm),
        (None, "<?xml version=\"1.0\"?><plist/>", ImportFormat::ITerm),
        (Some("X.JSON"), "", ImportFormat::WindowsTerminal),
        (
            None,
            "\u{feff}{\"background\": \"#000\"}",
            ImportFormat::WindowsTerminal,
        ),
        (Some("x.yaml"), "", ImportFormat::Base16),
        (
            None,
            "scheme: x\nbase00: \"000000\"\n",
            ImportFormat::Base16,
        ),
    ] {
        assert_eq!(detect_format(name, text), Some(format), "{name:?} {text:?}");
    }
    assert_eq!(detect_format(Some("notes.txt"), "just some prose\n"), None);
    assert_eq!(
        import_theme(Some("notes.txt"), b"just some prose\n", None),
        Err(ImportError::UnknownFormat)
    );
    assert_eq!(
        import_theme(Some("x.conf"), &[0xff, 0xfe, 0], None),
        Err(ImportError::NotUtf8)
    );
}

#[test]
fn a_theme_without_foreground_or_background_fails() {
    assert_eq!(
        convert(ImportFormat::Kitty, "foreground #ffffff\n", None),
        Err(ImportError::MissingColour {
            format: ImportFormat::Kitty,
            key: "background"
        })
    );
    assert!(matches!(
        convert(ImportFormat::Base16, "base00: \"000000\"\n", None),
        Err(ImportError::MissingColour {
            format: ImportFormat::Base16,
            ..
        })
    ));
}

#[test]
fn malformed_containers_fail_with_a_line() {
    for (format, text) in [
        (
            ImportFormat::WindowsTerminal,
            "{\n\"background\": \"#000\",\n\"x\": }",
        ),
        (
            ImportFormat::WindowsTerminal,
            "{\n\"name\": \"\\u+041\",\n\"background\": \"#000\"}",
        ),
        (
            ImportFormat::ITerm,
            "<plist>\n<dict>\n<key>x</key>\n<bogus/>",
        ),
        (
            ImportFormat::Alacritty,
            "[colors.primary\nforeground = '#fff'",
        ),
    ] {
        match convert(format, text, None) {
            Err(ImportError::Malformed { line, .. }) => assert!(line > 0, "{format:?}"),
            other => panic!("{format:?}: {other:?}"),
        }
    }
}

#[test]
fn nesting_is_bounded() {
    let deep = format!(
        "{}{}",
        "[".repeat(MAX_NESTING + 2),
        "]".repeat(MAX_NESTING + 2)
    );
    assert!(matches!(
        convert(ImportFormat::WindowsTerminal, &deep, None),
        Err(ImportError::Malformed { .. })
    ));
    let deep_plist = format!(
        "<plist>{}{}</plist>",
        "<array>".repeat(MAX_NESTING + 2),
        "</array>".repeat(MAX_NESTING + 2)
    );
    assert!(matches!(
        convert(ImportFormat::ITerm, &deep_plist, None),
        Err(ImportError::Malformed { .. })
    ));
    let deep_inline = format!(
        "a = {}'#000000'{}\n",
        "{ a = ".repeat(MAX_NESTING),
        " }".repeat(MAX_NESTING)
    );
    assert!(matches!(
        convert(ImportFormat::Alacritty, &deep_inline, None),
        Err(ImportError::Malformed { .. })
    ));
}

#[test]
fn unsafe_text_is_made_safe_and_still_round_trips() {
    let report = convert(
        ImportFormat::Kitty,
        &format!(
            "## name: Night #2 ; extra\u{1}\n## author: {}\nforeground #fff\nbackground #000\n",
            "x".repeat(MAX_THEME_TEXT_CHARS + 50)
        ),
        None,
    )
    .unwrap();
    assert_eq!(report.scheme.name, "Night#2; extra");
    assert_eq!(
        report.author.as_deref().map(|a| a.chars().count()),
        Some(MAX_THEME_TEXT_CHARS)
    );
    assert_round_trips(&report);
    for text in ["a #b", " #lead", "x\u{7}y", "  ", "#", "; a ;b"] {
        let once = sanitize_text(text);
        assert_eq!(once.as_deref().and_then(sanitize_text), once, "{text:?}");
    }
}

#[test]
fn unreadable_colours_are_dropped_with_a_note() {
    let report = convert(
        ImportFormat::Ghostty,
        "foreground = #fff\nbackground = #000\ncursor-color = cell-foreground\n",
        None,
    )
    .unwrap();
    assert_eq!(report.scheme.cursor, None);
    assert_eq!(report.dropped, ["cursor-color"]);
    assert!(report.notes.iter().any(|n| n.contains("cell-foreground")));
    assert!(report.notes.iter().any(|n| n.contains("defaults fill")));
}

#[test]
fn missing_ansi_colours_are_named_in_number() {
    let mut text = String::from("foreground #ffffff\nbackground #000000\n");
    for index in 0..14 {
        text.push_str(&format!("color{index} #101010\n"));
    }
    let report = convert(ImportFormat::Kitty, &text, None).unwrap();
    assert_eq!(
        report.notes,
        ["ANSI colours 14, 15 are not in the file: aterm's defaults fill them"]
    );
    text.push_str("color14 #101010\n");
    let report = convert(ImportFormat::Kitty, &text, None).unwrap();
    assert_eq!(
        report.notes,
        ["ANSI colour 15 is not in the file: aterm's default fills it"]
    );
}

#[test]
fn every_builtin_round_trips_through_both_serializer_forms() {
    for name in crate::scheme::builtin_names() {
        let catalog = crate::scheme::CatalogTheme::builtin(name).expect("a built-in");
        let theme = &catalog.theme;
        for write in [to_native_conf, to_native_conf_commented] {
            let text = write(
                &theme.scheme,
                theme.appearance_declared,
                theme.description.as_deref(),
                theme.author.as_deref(),
                &["a header"],
            );
            assert_eq!(
                parse_theme_str(&text).as_ref(),
                Ok(theme),
                "{name}:\n{text}"
            );
            let report = convert(ImportFormat::Native, &text, None).unwrap();
            assert_eq!(report.scheme, theme.scheme, "{name}");
            assert!(report.dropped.is_empty(), "{name}: {:?}", report.dropped);
        }
    }
}

#[test]
fn commented_form_uncommented_parses() {
    let scheme = ColorScheme {
        cursor: None,
        selection: None,
        ..ColorScheme::default()
    };
    let text = to_native_conf_commented(&scheme, false, None, None, &[]);
    let plain = parse_theme_str(&text).expect("the commented form parses");
    assert_eq!(plain.scheme, scheme);
    assert!(!plain.appearance_declared);
    let uncommented: String = text
        .lines()
        .map(|line| {
            let key = line
                .trim_start_matches("# ")
                .split(" = ")
                .next()
                .unwrap_or("");
            if line.starts_with("# ") && crate::scheme::lookup_theme_key(key).is_some() {
                line.trim_start_matches("# ").to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let full = parse_theme_str(&uncommented).expect("uncommented examples parse");
    assert!(full.appearance_declared);
    assert!(full.scheme.cursor.is_some() && full.scheme.selection.is_some());
    assert!(full.description.is_some() && full.author.is_some());
}
