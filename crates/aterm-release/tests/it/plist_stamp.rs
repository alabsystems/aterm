// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden tests for `bundle::stamp_info_plist` over `apps/aterm-mac/Info.plist`.

use crate::bundle;

fn real_template() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/aterm-mac/Info.plist"
    );
    std::fs::read_to_string(path).expect("read apps/aterm-mac/Info.plist")
}

fn stamp_real(icon: Option<&str>) -> String {
    bundle::stamp_info_plist(
        &real_template(),
        "0.2.0",
        1_783_918_101,
        "com.aterm.aterm",
        "aed5a06c1f00",
        icon,
    )
    .expect("stamp")
}

#[test]
fn stamps_the_three_versioned_keys_in_the_real_template() {
    let out = stamp_real(Some("aterm"));
    assert!(
        out.contains("<key>CFBundleShortVersionString</key>\n\t<string>0.2.0</string>"),
        "{out}"
    );
    assert!(
        out.contains("<key>CFBundleVersion</key>\n\t<string>1783918101</string>"),
        "{out}"
    );
    assert!(
        out.contains("<key>CFBundleIdentifier</key>\n\t<string>com.aterm.aterm</string>"),
        "{out}"
    );
    assert!(
        !out.contains("<key>CFBundleShortVersionString</key>\n\t<string>0.0.0</string>"),
        "baseline short version left behind: {out}"
    );
    assert!(
        !out.contains("<key>CFBundleVersion</key>\n\t<string>0</string>"),
        "baseline build number left behind: {out}"
    );
}

#[test]
fn inserts_atermgitcommit_and_iconfile_before_dict_end() {
    let out = stamp_real(Some("aterm"));
    assert!(
        out.contains("<key>ATermGitCommit</key>\n\t<string>aed5a06c1f00</string>"),
        "{out}"
    );
    assert!(
        out.contains("<key>CFBundleIconFile</key>\n\t<string>aterm</string>"),
        "{out}"
    );
    let dict_end = out.rfind("</dict>").unwrap();
    assert!(out.find("<key>ATermGitCommit</key>").unwrap() < dict_end);
    assert!(out.find("<key>CFBundleIconFile</key>").unwrap() < dict_end);
}

#[test]
fn icon_none_stamps_no_iconfile() {
    let out = stamp_real(None);
    assert!(!out.contains("CFBundleIconFile"), "{out}");
}

#[test]
fn stamping_twice_replaces_instead_of_duplicating() {
    let once = stamp_real(Some("aterm"));
    let twice = bundle::stamp_info_plist(
        &once,
        "0.3.0",
        1_783_918_102,
        "com.example.aterm",
        "bbbbbbbbbbbb-dirty",
        Some("aterm"),
    )
    .expect("re-stamp");
    assert_eq!(twice.matches("<key>ATermGitCommit</key>").count(), 1);
    assert_eq!(twice.matches("<key>CFBundleIconFile</key>").count(), 1);
    assert_eq!(twice.matches("<key>CFBundleVersion</key>").count(), 1);
    assert!(twice.contains("<string>1783918102</string>"));
    assert!(twice.contains("<string>bbbbbbbbbbbb-dirty</string>"));
    assert!(
        !twice.contains("1783918101"),
        "old build number left behind"
    );
}

#[test]
fn non_string_key_is_refused_not_misstamped() {
    let template = "<dict>\n\
                    \t<key>CFBundleVersion</key>\n\
                    \t<true/>\n\
                    \t<key>Other</key>\n\
                    \t<string>x</string>\n\
                    </dict>";
    let err = bundle::stamp_info_plist(template, "0.2.0", 1, "id", "c", None).unwrap_err();
    assert!(err.contains("CFBundleVersion"), "{err}");
}

#[test]
fn stamped_values_are_xml_escaped() {
    let template = "<dict>\n\t<key>CFBundleVersion</key>\n\t<string>0</string>\n</dict>";
    let out =
        bundle::stamp_info_plist(template, "0.2.0", 1, "a&b<c>", "commit", None).expect("stamp");
    assert!(out.contains("<string>a&amp;b&lt;c&gt;</string>"), "{out}");
}

#[test]
fn the_shipped_bundle_requires_native_execution() {
    let template = real_template();
    assert!(
        template.contains("<key>LSRequiresNativeExecution</key>\n\t<true/>"),
        "apps/aterm-mac/Info.plist must carry LSRequiresNativeExecution=true — \
         without it macOS may run aterm's Intel slice under Rosetta on Apple \
         silicon (v0.75.0 field report): {template}"
    );
    let out = stamp_real(Some("aterm"));
    assert!(
        out.contains("<key>LSRequiresNativeExecution</key>\n\t<true/>"),
        "stamping dropped LSRequiresNativeExecution: {out}"
    );
    assert!(
        !out.contains("<key>LSRequiresNativeExecution</key>\n\t<string>"),
        "LSRequiresNativeExecution must be <true/>, not a string: {out}"
    );
}
