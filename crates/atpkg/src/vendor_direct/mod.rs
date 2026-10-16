// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Vendor-direct agents (`docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`): the agent
//! programs ([`VENDORS`]) are fetched from their vendors, authenticated by compiled anchors, and
//! never wait on the ALab index. This module holds the table, the version ↔ build id map, the
//! decision, the lane's durable records, and the head watch ([`watch`]) the window runs (or, with
//! no window open, one terminal session).
//!
//! The index keeps one power over these programs: a signed yank (`policy`), which can deny the
//! installed version but never supply bytes.

pub(crate) mod decide;
pub(crate) mod digests;
pub(crate) mod durable;
pub(crate) mod lane;
pub mod policy;
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
pub use table::{
    AccountState, Anchor, DocPin, NEVER_INDEX_PINNED, VENDORS, VendorSpec, Warm, is_vendor,
    served_here, spec,
};
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

/// What a person has, for a line that names `program`'s active build offline (the self-update
/// announce, `selfupdate::announce_line`): a vendor-direct build id's version; for a legacy index
/// build, the version the program stamp recorded for it (`ProgramStamp::legacy_version_of`, the
/// verdict line's source too, so the two agree), else [`build_words`]; `(no version recorded)`
/// with no active build. Nothing is fetched or run.
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
        Some(v) => v.to_string(),
        None => build.to_string(),
    }
}

/// 64 lowercase hex digits: a sha256 as every vendor file spells it.
fn is_hex64(s: &str) -> bool {
    aterm_codec::hex::is_lower(s, 64)
}

#[cfg(test)]
mod wording_tests {
    use super::*;

    /// The display words render a vendor build as its version and an index build as
    /// its number.
    #[test]
    fn the_display_words_render_versions() {
        let id = Version::parse("2.1.280").unwrap().build_id();
        assert_eq!(build_label(id), "2.1.280");
        assert_eq!(build_words(id), "2.1.280");
        assert_eq!(build_words(4790), "build 4790");
        assert_eq!(display_build("claude", id), "claude 2.1.280");
        assert_eq!(display_build("trust", 4790), "trust build 4790");
        let muse = Version::parse("1.4.9-R6297.1").unwrap().build_id();
        assert_eq!(display_build("muse", muse), "muse 1.4.9-R6297.1");
    }
}
