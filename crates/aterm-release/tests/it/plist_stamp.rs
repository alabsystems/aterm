// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden tests for the Info.plist stamp (`bundle::stamp_info_plist`) over the committed
//! template `apps/aterm-mac/Info.plist`.

use crate::bundle;

/// The committed template, so template drift breaks the test, not the release.
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

// --- template goldens -------------------------------------------------------

#[test]
fn stamps_the_three_versioned_keys_in_the_real_template() {
    let out = stamp_real(Some("aterm"));
    // Replaced values, in the template's own tab-indented shape.
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
    // The template's real baseline values (`0.0.0`, build `0`) are gone.
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
    // Neither key is in the committed template: both are inserted before </dict>.
    let out = stamp_real(Some("aterm"));
    assert!(
        out.contains("<key>ATermGitCommit</key>\n\t<string>aed5a06c1f00</string>"),
        "{out}"
    );
    assert!(
        out.contains("<key>CFBundleIconFile</key>\n\t<string>aterm</string>"),
        "{out}"
    );
    // Inserted inside the dict.
    let dict_end = out.rfind("</dict>").unwrap();
    assert!(out.find("<key>ATermGitCommit</key>").unwrap() < dict_end);
    assert!(out.find("<key>CFBundleIconFile</key>").unwrap() < dict_end);
}

#[test]
fn icon_none_stamps_no_iconfile() {
    // The bundle step stamps CFBundleIconFile only when aterm.icns ships.
    let out = stamp_real(None);
    assert!(!out.contains("CFBundleIconFile"), "{out}");
}

#[test]
fn untouched_template_keys_survive_verbatim() {
    let out = stamp_real(Some("aterm"));
    // The updater reads min_os from LSMinimumSystemVersion; the executable name is the
    // menu-bar name.
    assert!(
        out.contains("<key>LSMinimumSystemVersion</key>\n\t<string>11.0</string>"),
        "{out}"
    );
    assert!(
        out.contains("<key>CFBundleExecutable</key>\n\t<string>aterm</string>"),
        "{out}"
    );
    assert!(out.contains("<key>NSHighResolutionCapable</key>"), "{out}");
}

/// The nine TCC usage strings, the music and photo libraries among them (a home-directory walk
/// raises them beside the folder prompts). They are static template keys `stamp_info_plist`
/// never names, so only template drift or a stamp rewriting the wrong `<string>` loses them.
const USAGE_KEYS: [&str; 9] = [
    "NSDocumentsFolderUsageDescription",
    "NSDesktopFolderUsageDescription",
    "NSDownloadsFolderUsageDescription",
    "NSNetworkVolumesUsageDescription",
    "NSRemovableVolumesUsageDescription",
    "NSFileProviderDomainUsageDescription",
    "NSAppDataUsageDescription",
    "NSAppleMusicUsageDescription",
    "NSPhotoLibraryUsageDescription",
];

#[test]
fn tcc_usage_strings_are_present_in_the_committed_template() {
    let template = real_template();
    for key in USAGE_KEYS {
        assert!(
            template.contains(&format!("<key>{key}</key>")),
            "apps/aterm-mac/Info.plist lost {key}"
        );
    }
}

#[test]
fn tcc_usage_strings_survive_stamping() {
    // The stamp is a textual replace-or-insert, so a drifted template could rewrite a
    // neighbouring <string>: each key keeps one non-empty sentence.
    let out = stamp_real(Some("aterm"));
    for key in USAGE_KEYS {
        let key_tag = format!("<key>{key}</key>");
        assert_eq!(
            out.matches(&key_tag).count(),
            1,
            "{key} duplicated or lost: {out}"
        );
        let after = out.find(&key_tag).expect("key present") + key_tag.len();
        let rest = &out[after..];
        let sstart = rest.find("<string>").expect("a <string> follows") + "<string>".len();
        let send = rest.find("</string>").expect("terminated <string>");
        let sentence = &rest[sstart..send];
        assert!(
            !rest[..sstart].contains("<key>"),
            "{key}'s <string> belongs to a later key: {out}"
        );
        // Each sentence names aterm as the cause, not Apple's bare default.
        assert!(
            sentence.contains("inside aterm"),
            "{key} no longer names aterm as the cause: {sentence}"
        );
        assert!(
            sentence.ends_with('.'),
            "{key} is not a sentence: {sentence}"
        );
    }
}

#[test]
fn tcc_usage_strings_survive_a_re_stamp() {
    // A re-stamp of a stamped plist leaves the static keys alone.
    let once = stamp_real(Some("aterm"));
    let twice = bundle::stamp_info_plist(
        &once,
        "0.3.0",
        1_783_918_102,
        "com.aterm.aterm.dev",
        "bbbbbbbbbbbb",
        Some("aterm"),
    )
    .expect("re-stamp");
    for key in USAGE_KEYS {
        assert_eq!(
            twice.matches(&format!("<key>{key}</key>")).count(),
            1,
            "{key} lost or duplicated on re-stamp: {twice}"
        );
    }
}

#[test]
fn stamping_twice_replaces_instead_of_duplicating() {
    // A re-stamp takes the "Set" path and never inserts a duplicate key.
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
    // A stamped key holding a non-<string> value is refused, never a rewrite of the next
    // key's <string>.
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

// --- provenance timestamp goldens -------------------------------------------

#[test]
fn epoch_to_rfc3339_goldens() {
    // Goldens computed with `date -u -r <epoch> +%Y-%m-%dT%H:%M:%SZ`.
    assert_eq!(bundle::epoch_to_rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        bundle::epoch_to_rfc3339(951_782_400),
        "2000-02-29T00:00:00Z"
    ); // leap day
    assert_eq!(
        bundle::epoch_to_rfc3339(1_783_354_739),
        "2026-07-06T16:18:59Z"
    ); // ledger seed
    assert_eq!(
        bundle::epoch_to_rfc3339(1_783_918_101),
        "2026-07-13T04:48:21Z"
    );
    assert_eq!(
        bundle::epoch_to_rfc3339(4_107_542_399),
        "2100-02-28T23:59:59Z"
    ); // 2100 ∉ leap
}

// --- launch policy ----------------------------------------------------------

/// The shipped bundle forbids Rosetta translation.
///
/// aterm is one universal binary, so on Apple silicon the native slice always exists. Without
/// the key LaunchServices may launch the Intel slice under translation (it inherits the
/// parent's architecture and remembers an "Open using Rosetta" tick), or refuse to open it
/// where Rosetta is absent. Asserted on the stamped output too, because the stamp is a textual
/// splice (`set_plist_string`) that could drop a neighbouring key.
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
    // A boolean: LaunchServices ignores `<string>true</string>`.
    assert!(
        !out.contains("<key>LSRequiresNativeExecution</key>\n\t<string>"),
        "LSRequiresNativeExecution must be <true/>, not a string: {out}"
    );
}

/// The shipped bundle carries NSSupportsAutomaticGraphicsSwitching=true, so a dual-GPU Mac's
/// mux may stay on the low-power chip aterm-gpu renders on (the template's comment says what
/// is and is not known about the key). Asserted on the stamped output too: it is the next key
/// after LSRequiresNativeExecution, and a textual splice could drop it.
#[test]
fn the_shipped_bundle_supports_automatic_graphics_switching() {
    let template = real_template();
    assert!(
        template.contains("<key>NSSupportsAutomaticGraphicsSwitching</key>\n\t<true/>"),
        "apps/aterm-mac/Info.plist must carry NSSupportsAutomaticGraphicsSwitching=true \
         — it is set so a dual-GPU Mac's mux may stay on the low-power GPU aterm \
         renders on (see this test's doc): {template}"
    );
    let out = stamp_real(Some("aterm"));
    assert!(
        out.contains("<key>NSSupportsAutomaticGraphicsSwitching</key>\n\t<true/>"),
        "stamping dropped NSSupportsAutomaticGraphicsSwitching: {out}"
    );
    // A boolean, as Apple documents the key; the stamp must not turn it into a string.
    assert!(
        !out.contains("<key>NSSupportsAutomaticGraphicsSwitching</key>\n\t<string>"),
        "NSSupportsAutomaticGraphicsSwitching must be <true/>, not a string: {out}"
    );
}
