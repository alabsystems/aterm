// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{simplified_app_name, token_for};

fn token(name: &str) -> Option<String> {
    token_for(&simplified_app_name(name))
}

#[test]
fn token_battery_matches_brew() {
    // Each row checked against brew 7.0.12 output.
    for (input, expected) in [
        ("Google Chrome", "google-chrome"),
        ("WhatsApp", "whatsapp"),
        ("PlayOnMac", "playonmac"),
        ("My App 2", "my-app-2"),
        ("Super Launcher", "super"),
        ("Frobnicator for Mac", "frobnicator"),
        ("Tool for macOS", "tool"),
        ("x86 Tool 64-bit", "x86-tool"),
        ("MP3 Gain 3", "mp3-gain-3"),
        ("Café Münster.app", "cafe-munster"),
        ("Easy MP3 Alarm Clock", "easy-mp3-alarm-clock"),
        ("Straße", "strae"),
        ("Łódź", "odz"),
        ("þing", "ing"),
        ("Æsop", "sop"),
        ("ﬁle", "file"),
    ] {
        assert_eq!(token(input).as_deref(), Some(expected), "{input}");
    }
    assert_eq!(token("  "), None);
}
