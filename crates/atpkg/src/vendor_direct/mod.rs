// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Vendor-direct agents (`docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`, Phase 1):
//! `claude` and `codex` are fetched from their vendors, authenticated by compiled anchors,
//! and never wait on the ALab index. This module is the pure core — the table, the
//! version ↔ build id map, the decision, the small durable records the lane keeps, and the
//! window's head watch ([`watch`]).
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
    s.push_str(&crate::dec_u64(build));
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
/// number for an index build. Hand-built (no `format!`) so `state.rs`, which builds its
/// strings by hand, can call it.
#[must_use]
pub fn build_label(build: u64) -> String {
    let Some(v) = Version::from_build_id(build) else {
        return crate::dec_u64(build);
    };
    let mut s = crate::dec_u64(u64::from(v.major()));
    s.push('.');
    s.push_str(&crate::dec_u64(u64::from(v.minor())));
    s.push('.');
    s.push_str(&crate::dec_u64(u64::from(v.patch())));
    s
}

/// 64 lowercase hex digits: a sha256 as every vendor file spells it.
fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The wording no line about a vendor-direct program may carry any more (design §1.7):
/// the index re-pin, the index as its update source, and the index pin itself.
#[cfg(test)]
pub(crate) const RETIRED_PHRASES: &[&str] = &[
    "within about an hour",
    "updates arrive with the ALab index",
    "pinned by aterm's signed index",
    "pinned by index",
];

/// `Some(what)` when `line` carries a [`RETIRED_PHRASES`] entry or a 19-digit number — a
/// vendor store id used as a label. A run right after `/` is a store directory in a path,
/// which is where that id belongs.
#[cfg(test)]
pub(crate) fn retired_wording(line: &str) -> Option<String> {
    if let Some(phrase) = RETIRED_PHRASES.iter().find(|p| line.contains(*p)) {
        return Some((*phrase).to_string());
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

    /// The checker finds each retired phrase and a bare store id, and passes a store path.
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
        for phrase in RETIRED_PHRASES {
            assert_eq!(
                retired_wording(&format!("x {phrase} y")).as_deref(),
                Some(*phrase)
            );
        }
        assert_eq!(build_label(id), "2.1.280");
        assert_eq!(build_words(id), "2.1.280");
        assert_eq!(build_words(4790), "build 4790");
        assert_eq!(display_build("claude", id), "claude 2.1.280");
        assert_eq!(display_build("trust", 4790), "trust build 4790");
    }
}
