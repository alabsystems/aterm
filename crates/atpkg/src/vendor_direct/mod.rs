// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Vendor-direct agent updates: vendor specs ([`VENDORS`]), version-to-build-id mapping,
//! durable records, yanked-version policy, and the head watch ([`watch`]).

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
pub(crate) use table::LEGACY_CEILING;
pub use table::{
    AccountState, Anchor, DocPin, NEVER_INDEX_PINNED, VENDORS, VendorSpec, Warm, is_vendor,
    served_here, spec,
};
pub use version::{BuildDate, VENDOR_BUILD_BASE, Version, is_vendor_build};

/// Format `program` and `build` for display (`claude 2.1.280` or `trust build 4790`).
#[must_use]
pub fn display_build(program: &str, build: u64) -> String {
    let mut s = String::from(program);
    s.push(' ');
    s.push_str(&build_words(build));
    s
}

/// Format `build` as a version string for vendor-direct builds or `build <n>` for index builds.
#[must_use]
pub fn build_words(build: u64) -> String {
    if Version::from_build_id(build).is_some() {
        return build_label(build);
    }
    let mut s = String::from("build ");
    s.push_str(&build.to_string());
    s
}

/// Format `program`'s active `build` offline using its vendor version, recorded legacy version, or [`build_words`].
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

/// Format `build` for a table column (version for vendor-direct builds, number for index builds).
#[must_use]
pub fn build_label(build: u64) -> String {
    match Version::from_build_id(build) {
        Some(v) => v.to_string(),
        None => build.to_string(),
    }
}

fn is_hex64(s: &str) -> bool {
    aterm_codec::hex::is_lower(s, 64)
}
