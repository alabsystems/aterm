// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_first_of_japanese_and_chinese_decides_and_neither_keeps_the_chinese_face_first() {
    assert!(!names_japanese_first([]));
    assert!(!names_japanese_first(["en-US"]));
    assert!(names_japanese_first(["en-US", "ja-JP", "zh-CN"]));
    assert!(names_japanese_first(["ja"]));
    assert!(!names_japanese_first(["zh-Hans-CN"]));
    assert!(!names_japanese_first(["en-GB", "ZH_TW", "ja-JP"]));
    assert!(!names_japanese_first(["jax-ZZ", "jv-ID", "zhx-ZZ"]));
}

#[test]
fn a_language_the_profile_alone_lists_decides() {
    // Display languages, then the profile's list, then the regional format: an English display
    // and region with Chinese (or Japanese) added for reading.
    assert!(!names_japanese_first([
        "en-US",
        "en-US",
        "zh-Hans-CN",
        "en-US"
    ]));
    assert!(names_japanese_first(["en-US", "en-US", "ja", "en-US"]));
    assert!(names_japanese_first(["en-US", "ja", "zh-Hans-CN"]));
    assert!(!names_japanese_first([
        "en-US",
        "zh-Hant-TW",
        "ja",
        "ja-JP"
    ]));
}

#[test]
fn a_nul_separated_list_reads_in_its_order() {
    let list: Vec<u16> = "en-US\0zh-Hans-CN\0\0".encode_utf16().collect();
    assert_eq!(names_of(&list), ["en-US", "zh-Hans-CN"]);
    assert!(names_of(&[]).is_empty());
    assert!(names_of(&[0, 0]).is_empty());
}

#[test]
fn this_machines_languages_are_read() {
    assert!(
        sys::default_locale_name().is_some_and(|name| !name.is_empty()),
        "the regional format has a name on every Windows"
    );
    assert!(
        !sys::preferred_ui_languages().is_empty(),
        "a user has at least one display language"
    );
    for name in sys::profile_languages() {
        assert!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{name:?} is not a language name"
        );
    }
    assert_eq!(user_reads_japanese_first(), user_reads_japanese_first());
}
