// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden tests for the Info.plist template substitution (release spec §6
//! `bundle.rs`), including the `-dirty` rule that must match
//! crates/aterm-gui/build.rs byte-for-byte — the plist's ATermGitCommit and
//! the binary's ATERM_GIT_COMMIT are compared by humans and scripts, so any
//! drift between the two stamps is a provenance bug.
//!
//! The module under test is mounted by path (aterm-release is a bin-only
//! crate; the pipeline modules are deliberately self-contained so tests can
//! compile them directly without a lib target).

#[path = "../src/seedpack.rs"]
#[allow(dead_code)]
mod seedpack;
#[path = "../src/bundle.rs"]
#[allow(dead_code)] // the test mount exercises the pure stamp/commit helpers only
mod bundle;

/// The COMMITTED template (apps/aterm-mac/Info.plist) — stamping goldens run
/// against the real thing so template drift breaks the test, not the release.
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
    // The committed baseline sentinels must be gone. These are the REAL values in
    // apps/aterm-mac/Info.plist (`0.0.0` short version, integer `0` build number);
    // asserting a value the template never contained would be vacuous.
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
    // Neither key exists in the committed template — both take PlistBuddy's
    // "Add" path: inserted before </dict>.
    let out = stamp_real(Some("aterm"));
    assert!(
        out.contains("<key>ATermGitCommit</key>\n\t<string>aed5a06c1f00</string>"),
        "{out}"
    );
    assert!(
        out.contains("<key>CFBundleIconFile</key>\n\t<string>aterm</string>"),
        "{out}"
    );
    // Inserted INSIDE the dict.
    let dict_end = out.rfind("</dict>").unwrap();
    assert!(out.find("<key>ATermGitCommit</key>").unwrap() < dict_end);
    assert!(out.find("<key>CFBundleIconFile</key>").unwrap() < dict_end);
}

#[test]
fn icon_none_stamps_no_iconfile() {
    // build-app.sh only stamps CFBundleIconFile when aterm.icns ships.
    let out = stamp_real(None);
    assert!(!out.contains("CFBundleIconFile"), "{out}");
}

#[test]
fn untouched_template_keys_survive_verbatim() {
    let out = stamp_real(Some("aterm"));
    // The updater reads min_os from LSMinimumSystemVersion; the executable
    // name is load-bearing for the menu-bar name. Neither may be disturbed.
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

#[test]
fn stamping_twice_replaces_instead_of_duplicating() {
    // A --resume re-entry may stamp an already-stamped plist: the second pass
    // must land on the "Set" path, never insert a duplicate key.
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
    // Template-drift guard: if a stamped key ever holds a non-<string> value
    // (e.g. <true/>), the stamp must refuse rather than rewrite the NEXT
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

// --- the -dirty rule (must match crates/aterm-gui/build.rs) ------------------

#[test]
fn dirty_rule_matches_build_rs() {
    // Clean commit → bare short hash.
    assert_eq!(
        bundle::commit_stamp(Some("aed5a06c1f00"), false),
        "aed5a06c1f00"
    );
    // Dirty tree on a real commit → "-dirty" suffix.
    assert_eq!(
        bundle::commit_stamp(Some("aed5a06c1f00"), true),
        "aed5a06c1f00-dirty"
    );
    // Unborn/.git-less tree stamps a bare "unknown", NEVER "unknown-dirty" —
    // build.rs only suffixes a REAL commit, and the plist must agree
    // byte-for-byte with the binary's own ATERM_GIT_COMMIT.
    assert_eq!(bundle::commit_stamp(None, false), "unknown");
    assert_eq!(bundle::commit_stamp(None, true), "unknown");
}

// --- provenance timestamp golden (build-app.sh's `date -u -r`) ---------------

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
