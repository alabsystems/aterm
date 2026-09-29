// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Vendor-direct agents (`docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`, Phase 1):
//! `claude` and `codex` are fetched from their vendors, authenticated by compiled anchors,
//! and never wait on the ALab index. This module is the pure core — the table, the
//! version ↔ build id map, the decision, the small durable records the lane keeps, and the
//! head watch ([`watch`]) the window runs — or, with no window open, one terminal session.
//!
//! The index keeps exactly one power over these programs: a signed yank (`policy`),
//! which can deny the installed version but never supply bytes.

pub(crate) mod decide;
pub(crate) mod digests;
pub(crate) mod durable;
pub(crate) mod lane;
pub(crate) mod policy;
mod record;
pub(crate) mod resolve;
mod stamp;
mod table;
mod version;
pub mod watch;

pub use record::{
    RECORD_SCHEMA, RECORD_SUFFIX, VendorRecord, complete_record, read_record, record_path,
};
pub use stamp::{ProgramStamp, stamp_path};
pub use table::{Anchor, DocPin, VENDORS, VendorSpec, is_vendor, spec};
pub use version::{BuildDate, VENDOR_BUILD_BASE, Version, is_vendor_build};

/// How a line names `program`'s store `build`: `claude 2.1.280` for a vendor-direct build
/// (never its 19-digit id), `trust build 4790` for an index build.
#[must_use]
pub fn display_build(program: &str, build: u64) -> String {
    let mut s = String::from(program);
    s.push(' ');
    s.push_str(&build_words(build));
    s
}

/// A store build as a sentence names it: `2.1.280` for a vendor-direct build, `build
/// 4790` for an index build.
#[must_use]
pub fn build_words(build: u64) -> String {
    if Version::from_build_id(build).is_some() {
        return build_label(build);
    }
    let mut s = String::from("build ");
    s.push_str(&build.to_string());
    s
}

/// What a person HAS, for a line that names `program`'s active build OFFLINE (the
/// self-update announce, `selfupdate::announce_line`): the version a vendor-direct build
/// id carries; for a legacy index build, the version an earlier pass recorded for it in
/// the program stamp (`ProgramStamp::legacy_version_of` — the verdict line's own source,
/// so the announce and the verdict agree on a `.vendor`-recorded install), else
/// [`build_words`]'s `build N`; `(no version recorded)` when there is no active build.
/// Nothing is fetched and nothing is run to learn it.
#[must_use]
pub fn have_words(layout: &crate::store::Layout, program: &str, build: Option<u64>) -> String {
    let Some(build) = build else {
        return String::from("(no version recorded)");
    };
    if Version::from_build_id(build).is_some() {
        return build_label(build);
    }
    if let Some(v) =
        stamp::ProgramStamp::read(layout, program).and_then(|stamp| stamp.legacy_version_of(build))
    {
        return v.to_string();
    }
    build_words(build)
}

/// A store build as a column shows it: the version for a vendor-direct build, the
/// number for an index build.
#[must_use]
pub fn build_label(build: u64) -> String {
    match Version::from_build_id(build) {
        Some(v) => format!("{}.{}.{}", v.major(), v.minor(), v.patch()),
        None => build.to_string(),
    }
}

/// 64 lowercase hex digits: a sha256 as every vendor file spells it.
fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The index-program words no line about a vendor-direct program may carry (design
/// §1.7): the index pin itself and the index as its update source. This crate still
/// prints both for index programs — `managed <build> — pinned by index <N>`
/// (`state::managed`) and doctor's fix-line `… (updates arrive with the ALab index)` — so
/// a vendor row routed through either renderer would say them. (The index-update era's
/// "within about an hour" and "pinned by aterm's signed index" have no writer left in
/// this crate and are not screened.)
#[cfg(test)]
pub(crate) const INDEX_PROGRAM_WORDS: &[&str] =
    &["pinned by index", "updates arrive with the ALab index"];

/// `Some(what)` when `line` carries an [`INDEX_PROGRAM_WORDS`] entry or a 19-digit number
/// — a vendor store id used as a label. A run right after `/` is a store directory in a
/// path, which is where that id belongs.
#[cfg(test)]
pub(crate) fn retired_wording(line: &str) -> Option<String> {
    if let Some(words) = INDEX_PROGRAM_WORDS.iter().find(|w| line.contains(*w)) {
        return Some((*words).to_string());
    }
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i - start >= 19 && (start == 0 || bytes[start - 1] != b'/') {
            return Some(line[start..i].to_string());
        }
    }
    None
}

#[cfg(test)]
mod wording_tests {
    use super::*;

    /// The checker finds each index-program phrase and a bare store id and passes a
    /// store path — the non-vacuity of every test that screens vendor lines with it —
    /// and the display words render versions.
    #[test]
    fn the_retired_wording_checker_finds_labels_and_passes_paths() {
        let id = Version::parse("2.1.280").unwrap().build_id();
        assert_eq!(
            retired_wording("claude 2.1.280 is Anthropic's latest"),
            None
        );
        assert_eq!(
            retired_wording(&format!("in /p/store/claude/{id}/bin (e.g. claude)")),
            None
        );
        assert_eq!(
            retired_wording(&format!("managed {id} — Anthropic latest")),
            Some(id.to_string())
        );
        for words in INDEX_PROGRAM_WORDS {
            assert_eq!(
                retired_wording(&format!("managed 2.1.280 — x {words} y")).as_deref(),
                Some(*words)
            );
        }
        assert_eq!(build_label(id), "2.1.280");
        assert_eq!(build_words(id), "2.1.280");
        assert_eq!(build_words(4790), "build 4790");
        assert_eq!(display_build("claude", id), "claude 2.1.280");
        assert_eq!(display_build("trust", 4790), "trust build 4790");
    }
}
