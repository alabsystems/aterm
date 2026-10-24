// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `<prefix>/vendor/<program>.toml`: persistent vendor-lane state ([`ProgramStamp`]).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::digests::AuthenticatedDigest;
use super::durable;
use super::table::is_vendor;
use super::version::Version;
use crate::store::Layout;

const STAMP_SCHEMA: u32 = 1;

const MAX_STAMP_BYTES: usize = 8 * 1024;

const MAX_ETAG_BYTES: usize = 256;

const MAX_VERDICT_CHARS: usize = 300;

/// Duration (in seconds) that a successful head check vouches for the vendor's latest build.
pub(crate) const CHECK_VOUCHES_FOR_SECS: i64 = 24 * 60 * 60;

/// Maximum future clock skew (in seconds) tolerated on a recorded head check.
pub(crate) const CHECK_CLOCK_SLACK_SECS: i64 = 60 * 60;

/// One program's stamp, as read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramStamp {
    /// When the head was last checked (Unix seconds).
    pub last_checked_at: i64,
    /// The head version last seen.
    pub last_head: Option<Version>,
    /// The head's `ETag`, sent back as `If-None-Match`.
    pub etag: Option<String>,
    /// The highest version this machine has installed, lowered only by a signed-yank move.
    pub high_water: Option<Version>,
    /// The lane's last verdict: one line, no control or bidi characters.
    pub verdict: String,
    /// Last stage refusal `(version, payload digest)` to avoid re-downloading.
    pub(crate) refused: Option<(Version, AuthenticatedDigest)>,
    /// Cached `(legacy index build, version)` read from its signed manifest.
    pub(crate) legacy: Option<(u64, Version)>,
    /// Cached `(legacy index build, signed tree_root)` for rollback attestation.
    pub(crate) legacy_root: Option<(u64, String)>,
}

#[derive(Serialize, Deserialize)]
struct StampFile {
    schema: u32,
    last_checked_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_head: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    high_water: Option<Version>,
    #[serde(default)]
    verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused_version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused_sha512: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_build: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_root_build: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_tree_root: Option<String>,
    /// Every key this build does not know.
    #[serde(flatten)]
    extra: BTreeMap<String, aterm_toml::Value>,
}

/// `<prefix>/vendor/<program>.toml`, for a vendor program only.
#[must_use]
pub fn stamp_path(layout: &Layout, program: &str) -> Option<PathBuf> {
    if !is_vendor(program) {
        return None;
    }
    Some(vendor_dir(layout).join(format!("{program}.toml")))
}

/// `<prefix>/vendor/`.
pub(super) fn vendor_dir(layout: &Layout) -> PathBuf {
    layout.prefix.join("vendor")
}

impl ProgramStamp {
    /// Reads `program`'s stamp, returning `None` if absent, unreadable, or malformed.
    #[must_use]
    pub fn read(layout: &Layout, program: &str) -> Option<Self> {
        read_file(layout, program).map(|file| Self::from_file(file).0)
    }

    fn from_file(file: StampFile) -> (Self, BTreeMap<String, aterm_toml::Value>) {
        let refused = file.refused_version.zip(AuthenticatedDigest::recorded(
            file.refused_sha256.as_deref().unwrap_or_default(),
            file.refused_sha512.as_deref().unwrap_or_default(),
        ));
        let legacy = match (file.legacy_build, file.legacy_version) {
            (Some(b), Some(v)) if !super::is_vendor_build(b) => Some((b, v)),
            _ => None,
        };
        let legacy_root = match (file.legacy_root_build, file.legacy_tree_root) {
            (Some(b), Some(root)) if !super::is_vendor_build(b) && super::is_hex64(&root) => {
                Some((b, root))
            }
            _ => None,
        };
        let stamp = Self {
            last_checked_at: file.last_checked_at,
            last_head: file.last_head,
            etag: file.etag.filter(|e| etag_ok(e)),
            high_water: file.high_water,
            verdict: one_line(&file.verdict),
            refused,
            legacy,
            legacy_root,
        };
        (stamp, file.extra)
    }

    /// Returns whether a head check succeeded within the validity window around `now`.
    #[must_use]
    pub(crate) fn checked_recently(&self, now: i64) -> bool {
        let age = now.saturating_sub(self.last_checked_at);
        self.last_checked_at > 0
            && (-CHECK_CLOCK_SLACK_SECS..=CHECK_VOUCHES_FOR_SECS).contains(&age)
    }

    /// The version legacy index build `build` carries, when an earlier pass read it.
    #[must_use]
    pub(crate) fn legacy_version_of(&self, build: u64) -> Option<Version> {
        self.legacy.filter(|(b, _)| *b == build).map(|(_, v)| v)
    }

    /// Records that legacy index build `build` carries `version`.
    /// # Errors
    /// `build` is a vendor build id, `program` is not vendor-direct, or the stamp could not be
    /// locked or written.
    pub(crate) fn record_legacy_version(
        layout: &Layout,
        program: &str,
        build: u64,
        version: Version,
    ) -> io::Result<()> {
        if super::is_vendor_build(build) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a vendor build names its own version",
            ));
        }
        update(layout, program, |s| s.legacy = Some((build, version)))
    }

    /// The signed root kept for legacy index build `build`, when one was.
    #[must_use]
    pub(crate) fn legacy_root_of(&self, build: u64) -> Option<&str> {
        self.legacy_root
            .as_ref()
            .filter(|(b, _)| *b == build)
            .map(|(_, root)| root.as_str())
    }

    /// Records the signed `root` for legacy index build `build`.
    /// # Errors
    /// `build` is a vendor build id, `root` is not 64 hex digits, `program` is not vendor-direct,
    /// or the stamp could not be locked or written.
    pub(crate) fn record_legacy_root(
        layout: &Layout,
        program: &str,
        build: u64,
        root: &str,
    ) -> io::Result<()> {
        let root = root.to_ascii_lowercase();
        if super::is_vendor_build(build) || !super::is_hex64(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a legacy build's tree root",
            ));
        }
        update(layout, program, |s| s.legacy_root = Some((build, root)))
    }

    /// Records a head check timestamp, head version, and `ETag`.
    /// # Errors
    /// `program` is not vendor-direct, or the stamp could not be locked or written.
    pub fn record_check(
        layout: &Layout,
        program: &str,
        checked_at: i64,
        head: Option<Version>,
        etag: Option<&str>,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.last_checked_at = checked_at;
            s.last_head = head;
            s.etag = etag.filter(|e| etag_ok(e)).map(str::to_string);
        })
    }

    /// Records the lane's `verdict` and raises `high_water` to `active`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn record_outcome(
        layout: &Layout,
        program: &str,
        verdict: &str,
        active: Option<Version>,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.verdict = one_line(verdict);
            if let Some(v) = active {
                s.high_water = Some(s.high_water.map_or(v, |hw| hw.max(v)));
            }
        })
    }

    /// Records a signed-yank move to `active`, setting `high_water` to `active`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn record_yank_move(
        layout: &Layout,
        program: &str,
        verdict: &str,
        active: Version,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.verdict = one_line(verdict);
            s.high_water = Some(active);
        })
    }

    /// Records a stage refusal of `version` with `digest`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn record_refusal(
        layout: &Layout,
        program: &str,
        verdict: &str,
        version: Version,
        digest: &AuthenticatedDigest,
    ) -> io::Result<()> {
        update(layout, program, |s| {
            s.verdict = one_line(verdict);
            s.refused = Some((version, digest.clone()));
        })
    }

    /// Clears any recorded stage refusal for `program`.
    /// # Errors
    /// Returns an error if `program` is not vendor-direct or the stamp cannot be written.
    pub(crate) fn clear_refusal(layout: &Layout, program: &str) -> io::Result<()> {
        if Self::read(layout, program).is_none_or(|s| s.refused.is_none()) {
            return Ok(());
        }
        update(layout, program, |s| s.refused = None)
    }

    /// Whether the lane refused `version` with `digest` at stage.
    #[must_use]
    pub(crate) fn is_refused(&self, version: Version, digest: &AuthenticatedDigest) -> bool {
        self.refused
            .as_ref()
            .is_some_and(|(v, d)| *v == version && d == digest)
    }
}

fn read_file(layout: &Layout, program: &str) -> Option<StampFile> {
    let file: StampFile = durable::read_toml(&stamp_path(layout, program)?, MAX_STAMP_BYTES)?;
    (file.schema == STAMP_SCHEMA).then_some(file)
}

fn update(
    layout: &Layout,
    program: &str,
    change: impl FnOnce(&mut ProgramStamp),
) -> io::Result<()> {
    let path = stamp_path(layout, program).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "not a vendor-direct program")
    })?;
    layout.ensure_dir(&vendor_dir(layout))?;
    let _lock = durable::lock_for_update(&path)?;
    let (mut stamp, extra) = read_file(layout, program)
        .map(ProgramStamp::from_file)
        .unwrap_or_default();
    change(&mut stamp);
    let (refused_version, (refused_sha256, refused_sha512)) =
        stamp.refused.map_or((None, (None, None)), |(v, digest)| {
            let (sha256, sha512) = digest.keyed();
            let set = |hex: String| (!hex.is_empty()).then_some(hex);
            (Some(v), (set(sha256), set(sha512)))
        });
    let (legacy_build, legacy_version) = stamp
        .legacy
        .map_or((None, None), |(b, v)| (Some(b), Some(v)));
    let (legacy_root_build, legacy_tree_root) = stamp
        .legacy_root
        .map_or((None, None), |(b, root)| (Some(b), Some(root)));
    let file = StampFile {
        schema: STAMP_SCHEMA,
        last_checked_at: stamp.last_checked_at,
        last_head: stamp.last_head,
        etag: stamp.etag.filter(|e| etag_ok(e)),
        high_water: stamp.high_water,
        verdict: one_line(&stamp.verdict),
        refused_version,
        refused_sha256,
        refused_sha512,
        legacy_build,
        legacy_version,
        legacy_root_build,
        legacy_tree_root,
        extra,
    };
    let text = durable::to_toml(&file, MAX_STAMP_BYTES)?;
    if std::fs::read(&path).is_ok_and(|have| have == text) {
        return Ok(());
    }
    durable::write_ordered(&path, &text)
}

fn etag_ok(etag: &str) -> bool {
    !etag.is_empty()
        && etag.len() <= MAX_ETAG_BYTES
        && etag.bytes().all(|b| (0x20..0x7f).contains(&b))
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if printable(c) { c } else { ' ' })
        .take(MAX_VERDICT_CHARS)
        .collect()
}

fn printable(c: char) -> bool {
    !c.is_control()
        && !matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[path = "stamp_corpus_tests.rs"]
#[cfg(test)]
mod corpus_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::DigestAlg;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-vendor-stamp-{label}-"))
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn digest(alg: DigestAlg, c: char) -> AuthenticatedDigest {
        let len = if alg == DigestAlg::Sha256 { 64 } else { 128 };
        AuthenticatedDigest::authenticated(alg, &c.to_string().repeat(len)).unwrap()
    }

    #[test]
    fn each_writer_changes_only_its_own_fields() {
        let l = layout("fields");
        assert_eq!(ProgramStamp::read(&l, "claude"), None);
        let recording = aterm_durable::seam::Recording::start();
        ProgramStamp::record_check(
            &l,
            "claude",
            1_790_000_000,
            Some(v("2.1.281")),
            Some("\"5f2c\""),
        )
        .unwrap();
        let flushes = recording.take();
        #[cfg(unix)]
        assert_eq!(
            flushes,
            [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered],
            "the stamp, file and entry"
        );
        #[cfg(not(unix))]
        assert!(
            matches!(
                flushes.as_slice(),
                [aterm_durable::Flush::Ordered]
                    | [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered]
            ),
            "the ordered file is required; the directory flush is best-effort: {flushes:?}"
        );
        drop(recording);
        ProgramStamp::record_outcome(&l, "claude", "installed 2.1.281", Some(v("2.1.281")))
            .unwrap();
        let expected = ProgramStamp {
            last_checked_at: 1_790_000_000,
            last_head: Some(v("2.1.281")),
            etag: Some("\"5f2c\"".into()),
            high_water: Some(v("2.1.281")),
            verdict: "installed 2.1.281".into(),
            refused: None,
            legacy: None,
            legacy_root: None,
        };
        assert_eq!(ProgramStamp::read(&l, "claude"), Some(expected.clone()));
        ProgramStamp::record_check(&l, "claude", 1_790_000_300, Some(v("2.1.282")), None).unwrap();
        let got = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(
            got,
            ProgramStamp {
                last_checked_at: 1_790_000_300,
                last_head: Some(v("2.1.282")),
                etag: None,
                ..expected
            }
        );
        assert_eq!(
            ProgramStamp::read(&l, "codex"),
            None,
            "stamps are per program"
        );
    }

    #[test]
    fn a_check_never_lowers_the_high_water() {
        let l = layout("stale");
        ProgramStamp::record_outcome(&l, "claude", "kept", Some(v("2.1.280"))).unwrap();
        let watch_copy = ProgramStamp::read(&l, "claude").unwrap();
        ProgramStamp::record_outcome(&l, "claude", "installed", Some(v("2.1.281"))).unwrap();
        ProgramStamp::record_check(&l, "claude", 9, watch_copy.last_head, Some("\"e\"")).unwrap();
        assert_eq!(
            ProgramStamp::read(&l, "claude").unwrap().high_water,
            Some(v("2.1.281"))
        );
    }

    #[test]
    fn the_high_water_rises_and_falls_only_by_a_yank_move() {
        let l = layout("hw");
        let hw = || ProgramStamp::read(&l, "codex").unwrap().high_water;
        ProgramStamp::record_outcome(&l, "codex", "a", Some(v("0.157.0"))).unwrap();
        ProgramStamp::record_outcome(&l, "codex", "b", Some(v("0.156.0"))).unwrap();
        assert_eq!(hw(), Some(v("0.157.0")));
        ProgramStamp::record_outcome(&l, "codex", "c", None).unwrap();
        assert_eq!(hw(), Some(v("0.157.0")), "no active build leaves the mark");
        ProgramStamp::record_yank_move(&l, "codex", "0.157.0 yanked", v("0.156.1")).unwrap();
        assert_eq!(hw(), Some(v("0.156.1")));
        ProgramStamp::record_outcome(&l, "codex", "d", Some(v("0.156.2"))).unwrap();
        assert_eq!(hw(), Some(v("0.156.2")));
    }

    #[test]
    fn concurrent_writers_lose_no_update() {
        let l = layout("race");
        let patches: Vec<u32> = (0..16).collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let patiently = |write: &dyn Fn() -> io::Result<()>| {
            loop {
                match write() {
                    Err(e)
                        if e.kind() == io::ErrorKind::TimedOut
                            && std::time::Instant::now() < deadline => {}
                    done => return done.unwrap(),
                }
            }
        };
        std::thread::scope(|s| {
            for &p in &patches {
                let (l, patiently) = (&l, &patiently);
                s.spawn(move || {
                    let ver = Version::new(2, 1, 300 + p).unwrap();
                    patiently(&|| ProgramStamp::record_outcome(l, "claude", "x", Some(ver)));
                    patiently(&|| {
                        ProgramStamp::record_check(l, "claude", i64::from(p), Some(ver), None)
                    });
                });
            }
        });
        assert_eq!(
            ProgramStamp::read(&l, "claude").unwrap().high_water,
            Some(v("2.1.315"))
        );
    }

    #[test]
    fn a_stage_refusal_is_memoized_per_version_and_digest() {
        let l = layout("refusal");
        let a = digest(DigestAlg::Sha256, 'a');
        ProgramStamp::record_outcome(&l, "codex", "kept", Some(v("0.156.0"))).unwrap();
        ProgramStamp::record_refusal(&l, "codex", "digest mismatch", v("0.157.0"), &a).unwrap();
        let got = ProgramStamp::read(&l, "codex").unwrap();
        assert!(got.is_refused(v("0.157.0"), &a));
        assert!(
            !got.is_refused(v("0.157.0"), &digest(DigestAlg::Sha256, 'b')),
            "a new digest is a new pair"
        );
        assert!(
            !got.is_refused(v("0.157.1"), &a),
            "a new head is a new pair"
        );
        assert_eq!(got.high_water, Some(v("0.156.0")));
        assert_eq!(got.verdict, "digest mismatch");
        let path = stamp_path(&l, "codex").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!("refused_sha256 = \"{}\"\n", sha('a'))),
            "{text}"
        );
        let torn: String = text
            .lines()
            .filter(|line| !line.starts_with("refused_sha256"))
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&path, torn).unwrap();
        assert_eq!(ProgramStamp::read(&l, "codex").unwrap().refused, None);
    }

    #[test]
    fn a_sha512_refusal_is_kept_under_its_own_key() {
        let l = layout("refusal-512");
        let five = digest(DigestAlg::Sha512, '5');
        ProgramStamp::record_refusal(&l, "codex", "signer refused", v("0.157.0"), &five).unwrap();
        let path = stamp_path(&l, "codex").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!("refused_sha512 = \"{}\"\n", "5".repeat(128))),
            "{text}"
        );
        assert!(!text.contains("refused_sha256"), "{text}");
        let got = ProgramStamp::read(&l, "codex").unwrap();
        assert!(got.is_refused(v("0.157.0"), &five));
        assert!(!got.is_refused(v("0.157.0"), &digest(DigestAlg::Sha512, '6')));
        std::fs::write(&path, format!("{text}refused_sha256 = \"{}\"\n", sha('5'))).unwrap();
        assert_eq!(ProgramStamp::read(&l, "codex").unwrap().refused, None);
    }

    #[test]
    fn the_explicit_door_forgets_a_refusal_and_nothing_else() {
        let l = layout("clear");
        ProgramStamp::clear_refusal(&l, "codex").unwrap();
        assert!(!vendor_dir(&l).exists(), "nothing to clear writes nothing");
        ProgramStamp::record_outcome(&l, "codex", "kept", Some(v("0.156.0"))).unwrap();
        let a = digest(DigestAlg::Sha256, 'a');
        ProgramStamp::record_refusal(&l, "codex", "digest mismatch", v("0.157.0"), &a).unwrap();
        ProgramStamp::clear_refusal(&l, "codex").unwrap();
        let got = ProgramStamp::read(&l, "codex").unwrap();
        assert_eq!(got.refused, None);
        assert_eq!(got.high_water, Some(v("0.156.0")));
        assert_eq!(got.verdict, "digest mismatch");
    }

    #[test]
    fn only_vendor_programs_have_a_stamp() {
        let l = layout("program");
        assert_eq!(stamp_path(&l, "trust"), None);
        assert_eq!(stamp_path(&l, "../claude"), None);
        assert!(ProgramStamp::record_check(&l, "digests", 1, None, None).is_err());
        assert!(ProgramStamp::record_outcome(&l, "trust", "x", None).is_err());
        assert!(!vendor_dir(&l).exists(), "a refused write creates nothing");
    }

    #[test]
    fn a_bad_etag_is_dropped_and_the_verdict_is_one_bounded_printable_line() {
        let l = layout("sanitize");
        ProgramStamp::record_check(&l, "claude", 1, None, Some("a\nb")).unwrap();
        assert_eq!(ProgramStamp::read(&l, "claude").unwrap().etag, None);
        let long = "e".repeat(MAX_ETAG_BYTES + 1);
        ProgramStamp::record_check(&l, "claude", 1, None, Some(&long)).unwrap();
        assert_eq!(ProgramStamp::read(&l, "claude").unwrap().etag, None);
        let verdict = format!("line one\nline two {}", "x".repeat(1000));
        ProgramStamp::record_outcome(&l, "claude", &verdict, None).unwrap();
        let got = ProgramStamp::read(&l, "claude").unwrap();
        assert!(got.verdict.starts_with("line one line two "));
        assert_eq!(got.verdict.chars().count(), MAX_VERDICT_CHARS);
    }

    #[test]
    fn the_verdict_carries_no_control_or_bidi_character() {
        assert_eq!(
            one_line(
                "a\u{1b}]0;title\u{7}b\u{9b}31m\tc\u{2028}d\u{2029}e\u{202e}f\u{2066}g\u{7f}h"
            ),
            "a ]0;title b 31m c d e f g h"
        );
        assert_eq!(
            one_line("claude 2.1.281 — Anthropic latest"),
            "claude 2.1.281 — Anthropic latest"
        );
        let l = layout("controls");
        ProgramStamp::record_outcome(&l, "claude", "ok", None).unwrap();
        let path = stamp_path(&l, "claude").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("verdict = \"ok\"", "verdict = \"x\\u001b[2Jy\""),
        )
        .unwrap();
        assert_eq!(ProgramStamp::read(&l, "claude").unwrap().verdict, "x [2Jy");
    }

    #[test]
    fn a_read_legacy_version_is_kept_by_its_build() {
        let l = layout("legacy");
        ProgramStamp::record_outcome(&l, "claude", "kept", None).unwrap();
        ProgramStamp::record_legacy_version(&l, "claude", 2_026_092_201, v("2.1.280")).unwrap();
        ProgramStamp::record_check(&l, "claude", 5, Some(v("2.1.281")), None).unwrap();
        let got = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(got.legacy_version_of(2_026_092_201), Some(v("2.1.280")));
        assert_eq!(got.legacy_version_of(2_026_091_901), None, "another build");
        assert_eq!(got.verdict, "kept");
        assert!(
            ProgramStamp::record_legacy_version(
                &l,
                "claude",
                v("2.1.280").build_id(),
                v("2.1.280")
            )
            .is_err()
        );
        ProgramStamp::record_legacy_version(&l, "claude", 2_026_092_301, v("2.1.281")).unwrap();
        let got = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(got.legacy_version_of(2_026_092_301), Some(v("2.1.281")));
        assert_eq!(got.legacy_version_of(2_026_092_201), None);
    }

    #[test]
    fn a_kept_legacy_root_is_kept_by_its_build() {
        let l = layout("legacy-root");
        ProgramStamp::record_legacy_version(&l, "claude", 2_026_092_201, v("2.1.280")).unwrap();
        ProgramStamp::record_legacy_root(&l, "claude", 2_026_091_901, &sha('A')).unwrap();
        let got = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(got.legacy_root_of(2_026_091_901), Some(sha('a').as_str()));
        assert_eq!(got.legacy_root_of(2_026_092_201), None, "another build");
        assert_eq!(got.legacy_version_of(2_026_092_201), Some(v("2.1.280")));
        let vendor = v("2.1.280").build_id();
        assert!(ProgramStamp::record_legacy_root(&l, "claude", vendor, &sha('b')).is_err());
        assert!(ProgramStamp::record_legacy_root(&l, "claude", 2_026_092_201, "abc").is_err());
        ProgramStamp::record_legacy_root(&l, "claude", 2_026_092_201, &sha('b')).unwrap();
        let got = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(got.legacy_root_of(2_026_092_201), Some(sha('b').as_str()));
        assert_eq!(got.legacy_root_of(2_026_091_901), None, "replaced");
        let path = stamp_path(&l, "claude").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace(&sha('b'), "not-a-root")).unwrap();
        assert_eq!(ProgramStamp::read(&l, "claude").unwrap().legacy_root, None);
    }

    #[test]
    fn a_key_this_build_does_not_know_survives_its_rewrites() {
        let l = layout("unknown-key");
        ProgramStamp::record_outcome(&l, "claude", "kept", Some(v("2.1.280"))).unwrap();
        let path = stamp_path(&l, "claude").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            format!("{text}later_field = \"kept\"\nlater_count = 3\n"),
        )
        .unwrap();
        ProgramStamp::record_check(&l, "claude", 9, Some(v("2.1.281")), Some("\"e\"")).unwrap();
        ProgramStamp::record_outcome(&l, "claude", "installed", Some(v("2.1.281"))).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("later_field = \"kept\""), "{text}");
        assert!(text.contains("later_count = 3"), "{text}");
        let got = ProgramStamp::read(&l, "claude").unwrap();
        assert_eq!(got.last_checked_at, 9);
        assert_eq!(got.high_water, Some(v("2.1.281")));
    }

    #[test]
    fn a_check_vouches_for_a_day() {
        let at = |checked: i64| ProgramStamp {
            last_checked_at: checked,
            ..ProgramStamp::default()
        };
        let now = 1_790_035_200;
        assert!(at(now).checked_recently(now));
        assert!(at(now - CHECK_VOUCHES_FOR_SECS).checked_recently(now));
        assert!(!at(now - CHECK_VOUCHES_FOR_SECS - 1).checked_recently(now));
        assert!(!at(0).checked_recently(now), "never checked");
        assert!(at(now + 60).checked_recently(now), "a clock set back");
        assert!(at(now + CHECK_CLOCK_SLACK_SECS).checked_recently(now));
        assert!(
            !at(now + CHECK_CLOCK_SLACK_SECS + 1).checked_recently(now),
            "a check stamped more than an hour ahead vouches for nothing"
        );
        assert!(!at(i64::MAX).checked_recently(now));
    }

    #[test]
    fn a_malformed_stamp_reads_as_absent_and_is_rewritten() {
        let l = layout("malformed");
        ProgramStamp::record_check(&l, "claude", 1, None, None).unwrap();
        let path = stamp_path(&l, "claude").unwrap();
        for bad in [
            "not toml {{{",
            "",
            "schema = 2\nlast_checked_at = 1\n",
            "schema = 1\nlast_checked_at = 1\nhigh_water = \"2.1.08\"\n",
            "schema = 1\n",
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(ProgramStamp::read(&l, "claude"), None, "{bad:?}");
        }
        ProgramStamp::record_check(&l, "claude", 7, None, None).unwrap();
        assert_eq!(
            ProgramStamp::read(&l, "claude"),
            Some(ProgramStamp {
                last_checked_at: 7,
                ..ProgramStamp::default()
            })
        );
    }
}
