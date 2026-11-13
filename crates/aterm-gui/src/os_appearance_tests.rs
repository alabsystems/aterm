// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::desktop::{
    parse_gsettings, parse_portal_read, parse_portal_signal, portal_color_scheme,
};
use super::{OsAppearanceSource, parse_appearance_word};
use aterm_types::Appearance;

#[test]
fn the_portal_mapping_covers_prefer_dark_prefer_light_and_none() {
    assert_eq!(portal_color_scheme(1), Some(Appearance::Dark));
    assert_eq!(portal_color_scheme(2), Some(Appearance::Light));
    assert_eq!(portal_color_scheme(0), Some(Appearance::Light));
    assert_eq!(
        portal_color_scheme(3),
        None,
        "negative control: not a portal value"
    );
}

#[test]
fn a_portal_read_parses_both_variant_depths() {
    assert_eq!(parse_portal_read("(<<uint32 1>>,)\n"), Some(1));
    assert_eq!(parse_portal_read("(<uint32 2>,)\n"), Some(2));
    assert_eq!(
        parse_portal_read(
            "Error: GDBus.Error:org.freedesktop.portal.Error.NotFound: Requested setting not found\n"
        ),
        None
    );
}

#[test]
fn a_portal_signal_is_read_only_for_the_color_scheme_key() {
    let line = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged \
                ('org.freedesktop.appearance', 'color-scheme', <uint32 1>)";
    assert_eq!(
        parse_portal_signal(line).and_then(portal_color_scheme),
        Some(Appearance::Dark)
    );
    let other = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged \
                 ('org.freedesktop.appearance', 'accent-color', <uint32 1>)";
    assert_eq!(
        parse_portal_signal(other),
        None,
        "negative control: another key"
    );
    assert_eq!(parse_portal_signal("(<<uint32 1>>,)"), None, "not a signal");
}

#[test]
fn the_gsettings_fallback_maps_gnome_color_scheme() {
    assert_eq!(parse_gsettings("'prefer-dark'\n"), Some(Appearance::Dark));
    assert_eq!(parse_gsettings("'prefer-light'\n"), Some(Appearance::Light));
    assert_eq!(parse_gsettings("'default'\n"), Some(Appearance::Light));
    assert_eq!(
        parse_gsettings("color-scheme: 'prefer-dark'"),
        Some(Appearance::Dark)
    );
    assert_eq!(parse_gsettings("No such key “color-scheme”\n"), None);
}

#[test]
fn the_inert_source_reads_nothing() {
    let inert = OsAppearanceSource::Inert;
    assert_eq!(inert.initial(), None);
    assert_eq!(
        inert.at_attach(|| Some(aterm_window::window::Theme::Light)),
        None
    );
    assert!(!inert.reads_window_theme());
    assert_eq!(OsAppearanceSource::choose(true, None), inert);
    assert_eq!(
        OsAppearanceSource::choose(false, None),
        OsAppearanceSource::Platform
    );
}

#[test]
fn an_injected_launch_reading_wins_and_observes_nothing() {
    for headless in [true, false] {
        let source = OsAppearanceSource::choose(headless, Some(Appearance::Light));
        assert_eq!(source, OsAppearanceSource::Injected(Appearance::Light));
        assert_eq!(source.initial(), Some(Appearance::Light));
        assert_eq!(
            source.at_attach(|| Some(aterm_window::window::Theme::Dark)),
            None
        );
        assert!(!source.reads_window_theme());
    }
}

#[test]
fn the_appearance_verb_parses_and_gates_injection() {
    use super::appearance_request;
    assert_eq!(appearance_request("", true), Ok(None));
    assert_eq!(
        appearance_request(" light ", true),
        Ok(Some(Appearance::Light))
    );
    assert_eq!(appearance_request("dark", true), Ok(Some(Appearance::Dark)));
    assert!(appearance_request("auto", true).is_err());
    assert!(appearance_request("dark light", true).is_err());
    assert_eq!(appearance_request("", false), Ok(None));
    assert!(
        appearance_request("light", false).is_err(),
        "negative control: a shipped build cannot inject"
    );
}

#[test]
fn appearance_words_round_trip() {
    for a in [Appearance::Dark, Appearance::Light] {
        assert_eq!(parse_appearance_word(super::appearance_word(a)), Some(a));
    }
    assert_eq!(parse_appearance_word("auto"), None);
}

/// The tether law: closing the tether ends the watched program, so a
/// watcher never outlives the process holding it.
#[cfg(unix)]
#[test]
fn closing_the_tether_ends_the_watcher() {
    // The watcher starts with the signals that end a process at their defaults, set here,
    // not this test binary's: one started ignoring the tether's signal (a `nohup` or `&`
    // runner's choice, which every exec keeps) outlives its tether.
    let start = aterm_spec::start::at_defaults(&aterm_spec::start::TERMINATING);
    let mut watcher =
        super::desktop::tethered("/bin/sh", &["-c", "while :; do echo tick; sleep 0.1; done"])
            .expect("spawn a tethered watcher");
    drop(start);
    for _ in 0..11 {
        let line = watcher.lines.next().expect("a line").expect("utf-8");
        assert_eq!(line, "tick", "the watcher runs while tethered");
    }
    watcher.release();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in watcher.lines.by_ref() {
            if line.is_err() {
                break;
            }
        }
        let _ = tx.send(());
        drop(watcher);
    });
    rx.recv_timeout(std::time::Duration::from_secs(60))
        .expect("the watcher ended when its tether closed");
}
