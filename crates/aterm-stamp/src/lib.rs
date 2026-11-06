// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The running binary's identity: one fixed-size record the `aterm` binary carries in its own
//! section and installs as the first statement of `main`.
//!
//! Product identity (the version, commit, build number, build time, dev-commit count and the
//! atpkg index hint) is data in the image, never an input of a library's compiled graph. Every
//! library reads it here, at run time ([`current`]), so a version bump or an index publication
//! recompiles only the `aterm` bin crate and relinks.
//!
//! The record is [`LEN`] bytes of text: the [`HEADER`] line, then one `key=value` line per
//! identity field in [`KEYS`] order, then NUL padding. `crates/aterm/build.rs` writes it with
//! [`encode`]; the binary's static holding it is read once with a volatile load ([`install`]), so
//! a writer that rewrites the section's bytes after the link is the one the binary reports.
//! [`rewrite`] is that writer: it finds the one record a linked image compiled in and replaces it.
//! The web module compiles in [`UNSTAMPED_RECORD`], which `ship site` rewrites with the version
//! and commit of the source it builds the module from.
//!
//! A process that installed no record (a library's own test binary, a dev helper) reads
//! [`Stamp::UNSTAMPED`]: version `0.0.0`, commit `unknown`. A test that needs a real identity
//! builds a [`Stamp`] and passes it where the code under test takes one.

use std::sync::OnceLock;

/// Size of the identity record, in bytes: the section's fixed length.
pub const LEN: usize = 512;

/// The record's first line; a section that does not start with it is not a stamp.
pub const HEADER: &str = "aterm-stamp 1\n";

/// The identity fields, in the order [`encode`] writes them.
pub const KEYS: [&str; 9] = [
    "version",
    "commit",
    "full_commit",
    "dirty",
    "built_at",
    "build",
    "dev_commits",
    "index_hint",
    "release",
];

/// The variables `crates/aterm/build.rs` writes a record from, and the only ones that name a
/// build. A release lane sets them for the `aterm` package; no library reads them.
pub mod input {
    /// The writer's mode: unset for a constant record (version only), `commit` for one stamped
    /// from git or from the pins below.
    pub const MODE: &str = "ATERM_STAMP";
    /// The pinned short commit (12 hex, `-dirty` suffixed for a changed tree).
    pub const COMMIT: &str = "ATERM_BUILD_GIT_COMMIT";
    /// The pinned full commit.
    pub const FULL_COMMIT: &str = "ATERM_BUILD_GIT_COMMIT_FULL";
    /// The pinned count of commits since the newest release tag.
    pub const DEV_COMMITS: &str = "ATERM_BUILD_DEV_COMMITS";
    /// The build number and build time, as a Unix epoch.
    pub const EPOCH: &str = "SOURCE_DATE_EPOCH";
    /// The shipped channel's published atpkg index run, for a fresh install's first discovery.
    pub const INDEX_HINT: &str = "ATERM_STAMP_INDEX_HINT";
    /// Set (to anything) by a release lane: the binary is a release product.
    pub const RELEASE: &str = "ATERM_RELEASE_BUILD";
    /// Every input above.
    pub const ALL: [&str; 7] = [
        MODE,
        COMMIT,
        FULL_COMMIT,
        DEV_COMMITS,
        EPOCH,
        INDEX_HINT,
        RELEASE,
    ];
}

/// The running binary's identity and the facts of its own compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// `[workspace.package] version`, as written.
    pub app_version: &'static str,
    /// Short commit (12 hex, `-dirty` suffixed for a tree with changes outside the measured
    /// ledger `docs/measured`, as `crates/aterm/stamp_paths.rs` says), or `"unknown"`.
    pub git_commit: &'static str,
    /// All forty hexadecimal commit characters, or `"unknown"`.
    pub git_commit_full: &'static str,
    /// `"true"` when [`Self::git_commit`] ends `-dirty`, else `"false"`.
    pub git_dirty: &'static str,
    /// UTC build timestamp (RFC3339), or `"unknown"`.
    pub build_time: &'static str,
    /// Monotonic build number: the release's ledger claim, else HEAD's committer epoch; `"0"`
    /// without either.
    pub build_number: &'static str,
    /// Commits since the newest release tag; `"0"` without git or the tag.
    pub dev_commits: &'static str,
    /// The published atpkg index run a release compiled in (`<low>..<newest>` or `<newest>`),
    /// or `""`.
    pub index_hint: &'static str,
    /// Whether a release lane produced the binary.
    pub release_build: bool,
    /// Full first line of the producing compiler's `-vV`.
    pub compiler_version_line: &'static str,
    /// The producing compiler's full commit hash, or `"unknown"`.
    pub compiler_commit: &'static str,
    /// The producing compiler's host triple, or `"unknown"`.
    pub compiler_host: &'static str,
    /// Trust's own version (the `trust:` line of `-vV`); `""` when the compiler reported none.
    pub compiler_trust_version: &'static str,
    /// `"r"` (upstream Rust) or `"t"` (Trust).
    pub compiler_flavor: &'static str,
    /// The cargo profile the binary was compiled under (`"debug"`/`"release"`).
    pub build_profile: &'static str,
    /// `"on"` iff the Trust verification pipeline was in the binary's compile.
    pub trust_verify: &'static str,
}

impl Stamp {
    /// The stamp of a process that installed none.
    pub const UNSTAMPED: Self = Self {
        app_version: "0.0.0",
        git_commit: "unknown",
        git_commit_full: "unknown",
        git_dirty: "false",
        build_time: "unknown",
        build_number: "0",
        dev_commits: "0",
        index_hint: "",
        release_build: false,
        compiler_version_line: "unknown",
        compiler_commit: "unknown",
        compiler_host: "unknown",
        compiler_trust_version: "",
        compiler_flavor: "r",
        build_profile: "unknown",
        trust_verify: "off",
    };

    /// `<version>+g<commit>`.
    #[must_use]
    pub fn build_id(&self) -> String {
        format!("{}+g{}", self.app_version, self.git_commit)
    }

    /// The release this stamp's version names, `(MAJOR, MINOR, PATCH)`: its core, before any
    /// pre-release or build metadata. `None` for a version that does not read as one.
    #[must_use]
    pub fn release(&self) -> Option<(u64, u64, u64)> {
        let core = self
            .app_version
            .split(['+', '-'])
            .next()
            .unwrap_or_default();
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => {
                Some((major, minor, patch))
            }
            _ => None,
        }
    }

    fn field(&self, key: &str) -> &str {
        match key {
            "version" => self.app_version,
            "commit" => self.git_commit,
            "full_commit" => self.git_commit_full,
            "dirty" => self.git_dirty,
            "built_at" => self.build_time,
            "build" => self.build_number,
            "dev_commits" => self.dev_commits,
            "index_hint" => self.index_hint,
            "release" if self.release_build => "true",
            "release" => "false",
            _ => "",
        }
    }
}

/// [`Stamp::UNSTAMPED`]'s record, byte for byte what [`encode`] writes for it: the record an image
/// compiles in when a writer stamps it after the link ([`rewrite`]).
pub const UNSTAMPED_RECORD: [u8; LEN] = record_of(
    "aterm-stamp 1\nversion=0.0.0\ncommit=unknown\nfull_commit=unknown\ndirty=false\n\
     built_at=unknown\nbuild=0\ndev_commits=0\nindex_hint=\nrelease=false\n",
);

/// `text` then NUL padding to [`LEN`] bytes; a text longer than [`LEN`] fails the build.
const fn record_of(text: &str) -> [u8; LEN] {
    let text = text.as_bytes();
    let mut out = [0u8; LEN];
    let mut i = 0;
    while i < text.len() {
        out[i] = text[i];
        i += 1;
    }
    out
}

/// Write `stamp`'s identity fields as a [`LEN`]-byte record.
/// # Errors
/// A value holding a newline or a NUL, or a record longer than [`LEN`].
pub fn encode(stamp: &Stamp) -> Result<[u8; LEN], String> {
    let mut out = [0u8; LEN];
    out[..HEADER.len()].copy_from_slice(HEADER.as_bytes());
    let mut len = HEADER.len();
    for key in KEYS {
        let value = stamp.field(key);
        if value.contains(['\n', '\0']) {
            return Err(format!("stamp {key} {value:?} holds a newline or a NUL"));
        }
        for part in [key.as_bytes(), b"=", value.as_bytes(), b"\n"] {
            if let Some(slot) = out.get_mut(len..len + part.len()) {
                slot.copy_from_slice(part);
            }
            len += part.len();
        }
    }
    if len > LEN {
        return Err(format!(
            "stamp record is {len} bytes, over the section's {LEN}"
        ));
    }
    Ok(out)
}

/// Read a record's identity fields over `compiled`, whose compile facts it keeps.
/// # Errors
/// A record without [`HEADER`], one that is not UTF-8 up to its padding, or one whose lines are
/// not exactly [`KEYS`] in order with `dirty` and `release` each `true` or `false`.
pub fn decode(record: &'static [u8; LEN], compiled: Stamp) -> Result<Stamp, String> {
    let mut stamp = compiled;
    for (key, value) in KEYS.into_iter().zip(values(record)?) {
        match key {
            "version" => stamp.app_version = value,
            "commit" => stamp.git_commit = value,
            "full_commit" => stamp.git_commit_full = value,
            "dirty" => stamp.git_dirty = value,
            "built_at" => stamp.build_time = value,
            "build" => stamp.build_number = value,
            "dev_commits" => stamp.dev_commits = value,
            "index_hint" => stamp.index_hint = value,
            _ => stamp.release_build = value == "true",
        }
    }
    Ok(stamp)
}

/// A record's values, in [`KEYS`] order, under [`decode`]'s rules.
fn values(record: &[u8; LEN]) -> Result<[&str; KEYS.len()], String> {
    let used = record.iter().position(|&b| b == 0).unwrap_or(LEN);
    let text = std::str::from_utf8(record.get(..used).unwrap_or_default())
        .map_err(|e| format!("stamp record is not UTF-8: {e}"))?;
    let body = text
        .strip_prefix(HEADER)
        .ok_or_else(|| format!("stamp record does not start {HEADER:?}"))?;
    let mut lines = body.split_terminator('\n');
    let mut values = [""; KEYS.len()];
    for (key, slot) in KEYS.into_iter().zip(&mut values) {
        let line = lines
            .next()
            .ok_or_else(|| format!("stamp record ends before {key}"))?;
        let value = line
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or_else(|| format!("stamp record line {line:?} is not {key}=…"))?;
        *slot = match key {
            "dirty" | "release" => boolean(key, value)?,
            _ => value,
        };
    }
    match lines.next() {
        None => Ok(values),
        Some(extra) => Err(format!("stamp record has an extra line {extra:?}")),
    }
}

/// Replace the one record a linked `image` compiled in with `stamp`'s: the post-link writer. A
/// record is [`LEN`] bytes from a [`HEADER`] that [`decode`] reads; any other bytes, a lone
/// [`HEADER`] string among them, are left alone.
/// # Errors
/// `stamp` does not encode ([`encode`]), or `image` holds no record or more than one, so which
/// to replace is not known; `image` is unchanged.
pub fn rewrite(image: &mut [u8], stamp: &Stamp) -> Result<(), String> {
    let record = encode(stamp)?;
    let header = HEADER.as_bytes();
    let mut found = Vec::new();
    for (at, window) in image.windows(header.len()).enumerate() {
        let is_record = window == header
            && image
                .get(at..at + LEN)
                .and_then(|r| <&[u8; LEN]>::try_from(r).ok())
                .is_some_and(|r| values(r).is_ok());
        if is_record {
            found.push(at);
        }
    }
    match *found.as_slice() {
        [at] => {
            if let Some(slot) = image.get_mut(at..at + LEN) {
                slot.copy_from_slice(&record);
            }
            Ok(())
        }
        [] => Err("the image holds no identity record".to_owned()),
        _ => Err(format!(
            "the image holds {} identity records, at offsets {found:?}",
            found.len()
        )),
    }
}

fn boolean<'a>(key: &str, value: &'a str) -> Result<&'a str, String> {
    match value {
        "true" | "false" => Ok(value),
        _ => Err(format!("stamp {key} is {value:?}, not true or false")),
    }
}

static INSTALLED: OnceLock<Stamp> = OnceLock::new();

/// Install the running binary's stamp from its section (first call wins): one volatile read of
/// `section`, so bytes written into the image after the link are the ones read, decoded over
/// `compiled`'s compile facts.
/// # Panics
/// When the section is not a record [`decode`] reads: the binary's own identity is corrupt, and
/// nothing it would report could be trusted.
pub fn install(section: &'static [u8; LEN], compiled: Stamp) {
    INSTALLED.get_or_init(|| {
        static RECORD: OnceLock<[u8; LEN]> = OnceLock::new();
        // SAFETY: `section` is a valid, aligned reference to an initialized `[u8; LEN]` that
        // lives for the whole process; a volatile read of it has no other precondition.
        let record = RECORD.get_or_init(|| unsafe { std::ptr::read_volatile(section) });
        match decode(record, compiled) {
            Ok(stamp) => stamp,
            Err(why) => panic!("this binary's identity section is corrupt: {why}"),
        }
    });
}

/// The installed stamp, or `None` in a process that installed none.
#[must_use]
pub fn installed() -> Option<&'static Stamp> {
    INSTALLED.get()
}

/// The installed stamp, else [`Stamp::UNSTAMPED`].
#[must_use]
pub fn current() -> &'static Stamp {
    installed().unwrap_or(&Stamp::UNSTAMPED)
}

/// The running binary's product version ([`current`]'s).
#[must_use]
pub fn app_version() -> &'static str {
    current().app_version
}

/// The running binary's `<version>+g<commit>` token.
#[must_use]
pub fn build_id() -> String {
    current().build_id()
}

#[cfg(test)]
mod tests;
