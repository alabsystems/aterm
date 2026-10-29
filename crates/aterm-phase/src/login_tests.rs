// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::prompt::fixtures::{
    API_RETRYING_401_2_1_310, FOOTER_NOT_LOGGED_IN_2_1_310, FOOTER_NOT_LOGGED_IN_MEASURED,
    LOGIN_CONSOLE_MENU, LOGIN_ERROR, LOGIN_EXPIRED, LOGIN_EXPIRED_2_1_310, LOGIN_HANDOFF,
    LOGIN_INTERRUPTED, LOGIN_INTERRUPTED_2_1_310, LOGIN_INTERRUPTED_AT_EXPIRED_2_1_310,
    LOGIN_INTERRUPTED_AT_WALL_2_1_310, LOGIN_INTERRUPTED_FULLSCREEN_AT_EXPIRED_2_1_310,
    LOGIN_OPENING, LOGIN_PICKER, LOGIN_PICKER_2_1_310, LOGIN_PICKER_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_CONSOLE, LOGIN_PICKER_CONSOLE_2_1_310,
    LOGIN_PICKER_FULLSCREEN_AGAIN_AT_EXPIRED_2_1_310, LOGIN_PICKER_FULLSCREEN_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310, LOGIN_PICKER_NO_LOGIN_2_1_310,
    LOGIN_PICKER_PLATFORM_2_1_310, LOGIN_PICKER_WARNED, LOGIN_PICKER_WARNED_2_1_310,
    LOGIN_RELOOPED, LOGIN_SUCCESS, WALL_401_RUN_LOGIN_2_1_310, WALL_INVALID_API_KEY_2_1_310,
    WALL_NOT_LOGGED_IN_2_1_310, WALL_NOT_LOGGED_IN_NO_LOGIN_2_1_310, WALL_OAUTH_REVOKED_2_1_310,
    WALL_SUBSCRIPTION_DISABLED_2_1_310, screen,
};
use crate::wall::{Placement, WallKind};

const NOT_LOGGED_IN: &str = "Not logged in · Please run /login";
const INVALID_KEY: &str = "Invalid API key · Fix external API key";
const EXPIRED: &str = "Login expired · Please run /login";
const FOOTER: &str = "Not logged in · Run /login";

/// One Claude Code build's measured `/login` captures, each with the lost login's words it was
/// typed at (`None`: no lost login is said above it).
struct Build {
    version: &'static str,
    /// The method picker as it opens, its focus on the first row.
    pickers: &'static [(&'static str, Option<&'static str>)],
    /// The picker, its focus moved by arrow keys alone (no method picked), and the method focused.
    moved: &'static [(&'static str, LoginMethod, Option<&'static str>)],
    /// Esc out of the picker: `⎿  Login interrupted` under `❯ /login`, the composer back.
    interrupted: &'static [(&'static str, Option<&'static str>)],
}

const BUILDS: &[Build] = &[
    Build {
        version: "2.1.303",
        pickers: &[
            (LOGIN_PICKER, None),
            (LOGIN_PICKER_WARNED, Some(NOT_LOGGED_IN)),
            (LOGIN_RELOOPED, Some(NOT_LOGGED_IN)),
        ],
        moved: &[(LOGIN_PICKER_CONSOLE, LoginMethod::Console, None)],
        interrupted: &[(LOGIN_INTERRUPTED, None)],
    },
    Build {
        version: "2.1.310",
        pickers: &[
            (LOGIN_PICKER_2_1_310, Some(INVALID_KEY)),
            (LOGIN_PICKER_WARNED_2_1_310, Some(NOT_LOGGED_IN)),
            (LOGIN_PICKER_NO_LOGIN_2_1_310, Some(NOT_LOGGED_IN)),
            (LOGIN_PICKER_AT_EXPIRED_2_1_310, Some(EXPIRED)),
            (LOGIN_PICKER_FULLSCREEN_AT_EXPIRED_2_1_310, Some(EXPIRED)),
            (
                LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310,
                Some(EXPIRED),
            ),
        ],
        moved: &[
            (
                LOGIN_PICKER_CONSOLE_2_1_310,
                LoginMethod::Console,
                Some(INVALID_KEY),
            ),
            (
                LOGIN_PICKER_PLATFORM_2_1_310,
                LoginMethod::Platform,
                Some(INVALID_KEY),
            ),
        ],
        interrupted: &[
            (LOGIN_INTERRUPTED_2_1_310, Some(INVALID_KEY)),
            (LOGIN_INTERRUPTED_AT_WALL_2_1_310, Some(NOT_LOGGED_IN)),
            (LOGIN_INTERRUPTED_AT_EXPIRED_2_1_310, Some(EXPIRED)),
            (
                LOGIN_INTERRUPTED_FULLSCREEN_AT_EXPIRED_2_1_310,
                Some(EXPIRED),
            ),
        ],
    },
];

/// The measured 2.1.310 screens a lost login leaves with the composer back and no `/login`
/// typed: the turn's own notice, where it is drawn, and the wall [`crate::wall::wall`] reads (the
/// footer's own notice first, then the turn's: `crate::phase::notice`).
const WALLS_2_1_310: &[(&str, &str, Placement, WallKind, &str, Placement)] = &[
    (
        WALL_NOT_LOGGED_IN_2_1_310,
        NOT_LOGGED_IN,
        Placement::Gutter,
        WallKind::Auth,
        NOT_LOGGED_IN,
        Placement::Gutter,
    ),
    (
        WALL_INVALID_API_KEY_2_1_310,
        INVALID_KEY,
        Placement::Gutter,
        WallKind::Auth,
        INVALID_KEY,
        Placement::Gutter,
    ),
    (
        WALL_OAUTH_REVOKED_2_1_310,
        "OAuth token revoked · Please run /login",
        Placement::Gutter,
        WallKind::Auth,
        "OAuth token revoked · Please run /login",
        Placement::Gutter,
    ),
    (
        WALL_401_RUN_LOGIN_2_1_310,
        "Please run /login · API Error: 401 Invalid bearer token",
        Placement::ErrorRow,
        WallKind::Auth,
        "Please run /login · API Error: 401 Invalid bearer token",
        Placement::ErrorRow,
    ),
    (
        WALL_NOT_LOGGED_IN_NO_LOGIN_2_1_310,
        NOT_LOGGED_IN,
        Placement::Gutter,
        WallKind::Auth,
        FOOTER,
        Placement::Footer,
    ),
    (
        LOGIN_EXPIRED_2_1_310,
        EXPIRED,
        Placement::ErrorRow,
        WallKind::Auth,
        FOOTER,
        Placement::Footer,
    ),
    (
        WALL_SUBSCRIPTION_DISABLED_2_1_310,
        "Your organization has disabled Claude subscription access for Claude Code · Use an \
         Anthropic API key instead, or ask your admin to enable access",
        Placement::ErrorRow,
        WallKind::Spend,
        "Your organization has disabled Claude subscription access for Claude Code · Use an \
         Anthropic API key instead, or ask your admin to enable access",
        Placement::ErrorRow,
    ),
];

/// The measured pickers drawn in the fullscreen renderer's bottom panel (2.1.310's, on its
/// default route); every other picker is the inline renderer's.
const FULLSCREEN: [&str; 3] = [
    LOGIN_PICKER_FULLSCREEN_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_AGAIN_AT_EXPIRED_2_1_310,
];

#[test]
fn every_measured_login_screen_is_read_as_its_state() {
    let picker = |text| match login_screen(&screen(text)) {
        Some(LoginScreen::Picker(p)) => p,
        other => panic!("{other:?}: {text}"),
    };
    let all = vec![
        LoginMethod::Subscription,
        LoginMethod::Console,
        LoginMethod::Platform,
    ];
    let frame = |text: &str| {
        if FULLSCREEN.contains(&text) {
            PanelRenderer::Fullscreen
        } else {
            PanelRenderer::Inline
        }
    };
    for b in BUILDS {
        for &(text, _) in b.pickers {
            let p = picker(text);
            assert_eq!(p.methods, all, "{}", b.version);
            assert_eq!(p.focused, Some(0), "{}: {text}", b.version);
            assert_eq!(p.title_row.trim(), "Login", "{}", b.version);
            assert_eq!(p.renderer, frame(text), "{}: {text}", b.version);
        }
        for &(text, method, _) in b.moved {
            let p = picker(text);
            assert_eq!(p.methods, all, "{}", b.version);
            assert_eq!(p.renderer, frame(text), "{}: {text}", b.version);
            assert_eq!(
                p.focused.map(|i| p.methods[i]),
                Some(method),
                "{}: {text}",
                b.version
            );
            assert_eq!(p.index_of(method), p.focused, "{}", b.version);
        }
    }
    assert_eq!(
        login_screen(&screen(LOGIN_CONSOLE_MENU)),
        Some(LoginScreen::ConsoleMenu)
    );
    for text in [LOGIN_OPENING, LOGIN_HANDOFF] {
        assert_eq!(login_screen(&screen(text)), Some(LoginScreen::Browser));
    }
    for id in ["login.no_browser", "login.paste"] {
        assert!(!LOGIN_OPENING.contains(crate::anchors::anchor(id)), "{id}");
    }
    assert!(LOGIN_OPENING.contains(crate::anchor!("login.opening")));
    assert_eq!(
        login_screen(&screen(LOGIN_ERROR)),
        Some(LoginScreen::Failed)
    );
    assert!(matches!(
        login_screen(&screen(LOGIN_SUCCESS)),
        Some(LoginScreen::Success { row }) if row.contains("Press Enter to continue")
    ));
}

/// The picker of each build is the prompt the approval loop answers: a box of the three methods'
/// rows in the order drawn, its focused option the picker's own, and no wall read under it.
#[test]
fn every_measured_picker_is_read_as_a_prompt_focused_as_drawn() {
    let all = [
        LoginMethod::Subscription,
        LoginMethod::Console,
        LoginMethod::Platform,
    ];
    for b in BUILDS {
        let opened = b
            .pickers
            .iter()
            .map(|&(text, _)| (text, LoginMethod::Subscription));
        for (text, focused) in opened.chain(b.moved.iter().map(|&(text, m, _)| (text, m))) {
            let r = crate::read(Some("claude"), &screen(text), None);
            assert_eq!(
                r.phase,
                crate::phase::Phase::Prompt,
                "{}: {text}",
                b.version
            );
            assert_eq!(r.wall, None, "{}: {text}", b.version);
            let prompt = r.prompt.expect("the picker's box");
            let labels: Vec<&str> = prompt.options.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(labels.len(), all.len(), "{}: {labels:?}", b.version);
            for (label, method) in labels.iter().zip(all) {
                assert!(label.starts_with(method.label()), "{}: {label}", b.version);
            }
            let at = prompt.focused().map(|o| o.label.as_str()).unwrap_or("");
            assert!(at.starts_with(focused.label()), "{}: {at}", b.version);
        }
    }
}

/// The fullscreen renderer's picker is read in its own frame: a bottom panel under a `▔` rule
/// (as it first opens, Claude Code's `◐ medium · /effort` hint drawn into that rule), its title
/// three columns in, and no `❯ /login` echo above it, the lost login said right above the panel;
/// a `/login` typed again over a left one (`⎿  Login interrupted` under `❯ /login` right above
/// the panel) is read at the same lost login, left before. Controls: its title moved to the
/// inline renderer's column, its rule opened by words, and the inline picker's `─` rule drawn
/// as a panel's `▔` are each no dialog.
#[test]
fn the_fullscreen_panel_is_read_in_its_own_frame_only() {
    for (text, left_before) in [
        (LOGIN_PICKER_FULLSCREEN_AT_EXPIRED_2_1_310, false),
        (LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310, false),
        (LOGIN_PICKER_FULLSCREEN_AGAIN_AT_EXPIRED_2_1_310, true),
    ] {
        let rows = screen(text);
        let top = rows
            .iter()
            .rposition(|r| r.starts_with('▔'))
            .expect("the panel's rule");
        assert_eq!(
            rows[..top]
                .iter()
                .filter(|r| r.trim_end() == "❯ /login")
                .count(),
            usize::from(left_before),
            "the fullscreen renderer echoes no /login above its panel, only one it left: {text}"
        );
        let Some(LoginScreen::Picker(p)) = login_screen(&rows) else {
            panic!("{rows:#?}");
        };
        assert_eq!(p.title_row, "   Login", "{text}");
        assert_eq!(p.renderer, PanelRenderer::Fullscreen, "{text}");
        assert_eq!(
            login_at_wall(&rows).map(|w| (w.message, w.left_before)),
            Some((EXPIRED.to_string(), left_before)),
            "{text}"
        );
        let title = top
            + 1
            + rows[top + 1..]
                .iter()
                .position(|r| !r.trim().is_empty())
                .unwrap();
        let mut inline_column = rows.clone();
        inline_column[title] = "  Login".to_string();
        let mut opened_by_words = rows.clone();
        opened_by_words[top] = format!("Update installed {}", "▔".repeat(120));
        for (what, changed) in [
            ("the title at column 2", &inline_column),
            ("words opening the rule", &opened_by_words),
        ] {
            assert_eq!(login_screen(changed), None, "{what}: {text}");
            assert_eq!(login_at_wall(changed), None, "{what}: {text}");
        }
    }
    let hinted = screen(LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310);
    assert!(
        hinted
            .iter()
            .any(|r| r.starts_with('▔') && r.contains("/effort")),
        "the measured hint sits in the panel's rule"
    );
    let inline = screen(LOGIN_PICKER_AT_EXPIRED_2_1_310);
    assert!(matches!(
        login_screen(&inline),
        Some(LoginScreen::Picker(_))
    ));
    let mut paneled = inline.clone();
    let rule = paneled
        .iter()
        .rposition(|r| r.starts_with('─'))
        .expect("the inline rule");
    paneled[rule] = "▔".repeat(144);
    assert_eq!(
        login_screen(&paneled),
        None,
        "the inline title under a panel's rule"
    );
}

#[test]
fn a_screen_with_a_composer_is_no_login_dialog() {
    let interrupted = BUILDS
        .iter()
        .flat_map(|b| b.interrupted.iter().map(|i| i.0));
    let walls = WALLS_2_1_310.iter().map(|w| w.0);
    let others = [
        LOGIN_EXPIRED,
        FOOTER_NOT_LOGGED_IN_MEASURED,
        FOOTER_NOT_LOGGED_IN_2_1_310,
        API_RETRYING_401_2_1_310,
    ];
    for text in interrupted.chain(walls).chain(others) {
        assert_eq!(login_screen(&screen(text)), None, "{text}");
    }
}

#[test]
fn esc_out_of_the_dialog_is_read_as_the_login_interrupted() {
    for b in BUILDS {
        for &(text, _) in b.interrupted {
            assert!(login_interrupted(&screen(text)), "{}: {text}", b.version);
        }
        for text in b
            .pickers
            .iter()
            .map(|p| p.0)
            .chain(b.moved.iter().map(|m| m.0))
        {
            assert!(!login_interrupted(&screen(text)), "{}: {text}", b.version);
        }
    }
    let walls = WALLS_2_1_310.iter().map(|w| w.0);
    for text in [LOGIN_EXPIRED, LOGIN_HANDOFF, API_RETRYING_401_2_1_310]
        .into_iter()
        .chain(walls)
    {
        assert!(!login_interrupted(&screen(text)), "{text}");
    }
}

#[test]
fn a_login_typed_at_a_lost_login_is_read_as_the_walls() {
    let at_wall = |rows: &[String]| login_at_wall(rows);
    for b in BUILDS {
        let typed = b
            .pickers
            .iter()
            .chain(b.interrupted)
            .copied()
            .chain(b.moved.iter().map(|&(text, _, wall)| (text, wall)));
        for (text, wall) in typed {
            let got = at_wall(&screen(text));
            assert_eq!(
                got.as_ref().map(|w| w.message.as_str()),
                wall,
                "{}: {text}",
                b.version
            );
            assert!(got.is_none_or(|w| !w.left_before), "{}: {text}", b.version);
        }
    }
    for text in [LOGIN_OPENING, LOGIN_HANDOFF, LOGIN_ERROR, LOGIN_SUCCESS] {
        let got = at_wall(&screen(text)).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(got.message, NOT_LOGGED_IN, "{got:?}");
        assert!(!got.left_before, "{text}");
    }
    let handoff = screen(LOGIN_HANDOFF);
    let typed = handoff
        .iter()
        .position(|r| r.trim_end() == "❯ /login")
        .expect("the /login row");
    let mut left: Vec<String> = handoff[..=typed].to_vec();
    left.extend(
        [
            "  ⎿  Login interrupted",
            "",
            &"─".repeat(144),
            "❯ ",
            &"─".repeat(144),
        ]
        .map(str::to_string),
    );
    assert_eq!(
        at_wall(&left).map(|w| w.left_before),
        Some(false),
        "{left:#?}"
    );
    let mut again: Vec<String> = handoff[..=typed].to_vec();
    again.push("  ⎿  Login interrupted".to_string());
    again.push(String::new());
    again.extend(handoff[typed..].iter().cloned());
    assert_eq!(at_wall(&again).map(|w| w.left_before), Some(true));
    let measured_again = screen(LOGIN_INTERRUPTED_AT_WALL_2_1_310);
    let mut reopened: Vec<String> = measured_again
        .iter()
        .take_while(|r| !r.starts_with('─'))
        .cloned()
        .collect();
    let dialog = screen(LOGIN_PICKER_WARNED_2_1_310);
    let typed = dialog
        .iter()
        .position(|r| r.trim_end() == "❯ /login")
        .expect("the /login row");
    reopened.push(String::new());
    reopened.extend(dialog[typed..].iter().cloned());
    assert_eq!(
        at_wall(&reopened).map(|w| (w.message, w.left_before)),
        Some((NOT_LOGGED_IN.to_string(), true)),
        "the measured 2.1.310 rows, /login typed again: {reopened:#?}"
    );
    let walls = WALLS_2_1_310.iter().map(|w| w.0);
    for text in [LOGIN_CONSOLE_MENU, LOGIN_EXPIRED].into_iter().chain(walls) {
        assert_eq!(at_wall(&screen(text)), None, "{text}");
    }
}

/// Every measured 2.1.310 lost login with the composer back: the turn's own notice is the vendor
/// row it was measured as, its words the wall's kind (`classify_wall`), and the wall read is the
/// footer's own `Not logged in · Run /login` where Claude Code draws one (no login at all),
/// else the turn's notice. Claude Code's refusal of the subscription by the organization reads
/// as the account-blocked wall the phrase table names it (`Tag::Blocked`, [`WallKind::Spend`]),
/// not a lost login. A 401 that an `apiKeyHelper` key's retries are still riding is no wall yet.
#[test]
fn every_measured_lost_login_is_read_as_its_wall() {
    for &(text, said, at, kind, read, placed) in WALLS_2_1_310 {
        let rows = screen(text);
        let notice = crate::phase::last_said_notice(&rows).expect("the turn's own notice");
        assert_eq!(
            (notice.text.as_str(), notice.placement),
            (said, at),
            "{text}"
        );
        assert_eq!(crate::wall::classify_wall(said), Some(kind), "{said}");
        let w = crate::wall::wall(&rows).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(
            (w.kind, w.message.as_str(), w.placement),
            (kind, read, placed),
            "{text}"
        );
    }
    for text in [FOOTER_NOT_LOGGED_IN_MEASURED, FOOTER_NOT_LOGGED_IN_2_1_310] {
        let w = crate::wall::wall(&screen(text)).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(
            (w.kind, w.message.as_str(), w.placement),
            (WallKind::Auth, FOOTER, Placement::Footer),
            "{text}"
        );
    }
    let retrying = screen(API_RETRYING_401_2_1_310);
    assert_eq!(crate::wall::wall(&retrying), None);
    assert_eq!(
        crate::read(Some("claude"), &retrying, None).phase,
        crate::phase::Phase::Busy
    );
}
