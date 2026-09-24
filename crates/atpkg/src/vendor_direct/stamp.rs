// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `<prefix>/vendor/<program>.toml`: what the vendor lane remembers about one program —
//! the head watch's last look (when, what, and the `ETag` that makes the next look a 304),
//! the machine's high-water, the lane's last verdict, the last stage refusal, the
//! version a legacy index build carries once it was read, and the signed root of the
//! legacy build a vendor install replaced.
//!
//! The head watch and the lane run in different processes and own different fields, so
//! every write is a read-modify-write of its own fields under the file's lock: a writer
//! holding a stale copy can never lower the high-water. Losing the file costs one full
//! fetch, never safety: the installed build is its own floor.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::durable;
use super::table::is_vendor;
use super::version::Version;
use crate::store::Layout;

/// The stamp file's schema.
const STAMP_SCHEMA: u32 = 1;

/// Bound on a stamp read.
const MAX_STAMP_BYTES: usize = 8 * 1024;

/// The longest `ETag` kept; a longer one is dropped (the next check is a full GET).
const MAX_ETAG_BYTES: usize = 256;

/// The longest verdict kept, in characters.
const MAX_VERDICT_CHARS: usize = 300;

/// How long a successful head check vouches for "the vendor's latest" (seconds): past it,
/// no surface calls the build the vendor's latest until a check reaches the vendor again.
pub(crate) const CHECK_VOUCHES_FOR_SECS: i64 = 24 * 60 * 60;

/// How far ahead of the clock a check may be stamped and still vouch (seconds): a clock
/// set back a little is still the same day's check; one stamped further ahead is a clock
/// that was wrong when it was written, and vouches for nothing.
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
    /// The last stage refusal, `(version, sha256)`: that pair is not downloaded again.
    pub(crate) refused: Option<(Version, String)>,
    /// `(legacy index build, the version its signed manifest names)`, read once so no later
    /// pass needs an index to know it.
    pub(crate) legacy: Option<(u64, Version)>,
    /// `(legacy index build, its signed tree_root)`: the recorded row's root, kept when a
    /// vendor build replaced that row, or the root its signed manifest names. What attests
    /// the build after a rollback to it, with no index asked.
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
    legacy_build: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_root_build: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_tree_root: Option<String>,
    /// Every key this build does not know, carried verbatim through each rewrite: a build
    /// that rewrote the file from its own fields erased what a newer one kept (the legacy
    /// root, which the lane then had to read from the signed manifest again).
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
    /// `program`'s stamp; `None` when absent, unreadable or malformed (treated as never
    /// checked).
    #[must_use]
    pub fn read(layout: &Layout, program: &str) -> Option<Self> {
        read_file(layout, program).map(|file| Self::from_file(file).0)
    }

    /// The stamp `file` holds, and the keys this build does not know.
    fn from_file(file: StampFile) -> (Self, BTreeMap<String, aterm_toml::Value>) {
        let refused = match (file.refused_version, file.refused_sha256) {
            (Some(v), Some(sha)) if super::is_hex64(&sha) => Some((v, sha)),
            _ => None,
        };
        // A vendor build id names its own version; only an index build number is legacy.
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

    /// Whether a head check reached the vendor within [`CHECK_VOUCHES_FOR_SECS`] before
    /// `now`, or at most [`CHECK_CLOCK_SLACK_SECS`] after it: only then may a surface call
    /// the build it left the vendor's latest. Never checked is not recent, and neither is
    /// a check stamped further in the future.
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

    /// Record that legacy index build `build` carries `version` (read from its signed
    /// manifest), replacing the one an older legacy build was recorded with.
    ///
    /// # Errors
    /// `build` is a vendor build id, `program` is not vendor-direct, or the stamp could not
    /// be locked or written.
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

    /// Keep legacy index build `build`'s signed `root` — a recorded row's, or its signed
    /// manifest's — replacing the one an older legacy build was kept with.
    ///
    /// # Errors
    /// `build` is a vendor build id, `root` is not 64 hex digits, `program` is not
    /// vendor-direct, or the stamp could not be locked or written.
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

    /// Record one head check — when, the head seen, and its `ETag` (one that is not short
    /// printable ASCII is dropped) — keeping every other field as the file holds it.
    ///
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

    /// Record the lane's verdict for a pass that left `active` as the active version, and
    /// raise the high-water to it.
    ///
    /// # Errors
    /// `program` is not vendor-direct, or the stamp could not be locked or written.
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

    /// Record a signed-yank move (a replacement or rollback off a yanked build) to
    /// `active`. The high-water becomes `active` even when that lowers it: a signature
    /// moved the machine there, and a mark left on the yanked version would refuse every
    /// later head between the two.
    ///
    /// # Errors
    /// `program` is not vendor-direct, or the stamp could not be locked or written.
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

    /// Record a stage refusal (a digest mismatch or a failed codesign check) of `version`
    /// with `sha256`, so the lane does not download that pair again; a new head or a new
    /// digest is a new pair.
    ///
    /// # Errors
    /// `sha256` is not 64 hex digits, `program` is not vendor-direct, or the stamp could
    /// not be locked or written.
    pub(crate) fn record_refusal(
        layout: &Layout,
        program: &str,
        verdict: &str,
        version: Version,
        sha256: &str,
    ) -> io::Result<()> {
        let sha256 = sha256.to_ascii_lowercase();
        if !super::is_hex64(&sha256) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refused digest is not a sha256",
            ));
        }
        update(layout, program, |s| {
            s.verdict = one_line(verdict);
            s.refused = Some((version, sha256));
        })
    }

    /// Forget the stage refusal: the explicit door (a person asking again) re-fetches
    /// what an unattended pass would not.
    ///
    /// # Errors
    /// `program` is not vendor-direct, or the stamp could not be locked or written.
    pub(crate) fn clear_refusal(layout: &Layout, program: &str) -> io::Result<()> {
        if Self::read(layout, program).is_none_or(|s| s.refused.is_none()) {
            return Ok(());
        }
        update(layout, program, |s| s.refused = None)
    }

    /// Whether the lane refused `version` with `sha256` at stage.
    #[must_use]
    pub(crate) fn is_refused(&self, version: Version, sha256: &str) -> bool {
        self.refused
            .as_ref()
            .is_some_and(|(v, sha)| *v == version && sha.eq_ignore_ascii_case(sha256))
    }
}

/// `program`'s stamp file as written, at this build's schema; `None` when absent,
/// unreadable, malformed or of another schema.
fn read_file(layout: &Layout, program: &str) -> Option<StampFile> {
    let file: StampFile = durable::read_toml(&stamp_path(layout, program)?, MAX_STAMP_BYTES)?;
    (file.schema == STAMP_SCHEMA).then_some(file)
}

/// Read `program`'s stamp (absent or malformed reads as empty), apply `change`, and
/// durably replace it — keys this build does not know included — all under the stamp's
/// lock.
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
    let (refused_version, refused_sha256) = stamp
        .refused
        .map_or((None, None), |(v, sha)| (Some(v), Some(sha)));
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
        legacy_build,
        legacy_version,
        legacy_root_build,
        legacy_tree_root,
        extra,
    };
    let text = durable::to_toml(&file, MAX_STAMP_BYTES)?;
    // An idle pass re-records the verdict it recorded last time: the same bytes are
    // not written again (Phase 3 — no redundant write in an idle pass).
    if std::fs::read(&path).is_ok_and(|have| have == text) {
        return Ok(());
    }
    durable::write_durable(&path, &text)
}

/// A header value we will send back verbatim: 1–256 bytes of printable ASCII.
fn etag_ok(etag: &str) -> bool {
    !etag.is_empty()
        && etag.len() <= MAX_ETAG_BYTES
        && etag.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// `text` as one line that is safe to print into a terminal: every control character
/// (C0, DEL, C1 — so no escape sequence), line or paragraph separator and bidi control
/// becomes a space, and the result is cut to [`MAX_VERDICT_CHARS`].
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if printable(c) { c } else { ' ' })
        .take(MAX_VERDICT_CHARS)
        .collect()
}

/// Not a control, separator or bidi-control character.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(label: &str) -> Layout {
        let p =
            std::env::temp_dir().join(format!("atpkg-vendor-stamp-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Layout { prefix: p }
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(64)
    }

    #[test]
    fn each_writer_changes_only_its_own_fields() {
        let l = layout("fields");
        assert_eq!(ProgramStamp::read(&l, "claude"), None);
        ProgramStamp::record_check(
            &l,
            "claude",
            1_790_000_000,
            Some(v("2.1.281")),
            Some("\"5f2c\""),
        )
        .unwrap();
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
        // A later check replaces only the check's fields.
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

    /// The finding this shape exists for: the head watch holds an old copy while the lane
    /// raises the high-water; the watch's next write must not lower it.
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

    /// Writers in parallel (threads here; processes take the same `flock`) lose no update.
    /// A writer the lock's bounded wait refuses writes again: 32 durable writes (each a
    /// file and a directory flush) queue behind one lock, and on a loaded disk the queue
    /// alone took 0.8–4.9 s (measured 2026-09-23) against the 5 s bound — the claim is that
    /// no write is lost, not how long the queue is, and a refusal is never a lost write.
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
        ProgramStamp::record_outcome(&l, "codex", "kept", Some(v("0.156.0"))).unwrap();
        ProgramStamp::record_refusal(&l, "codex", "digest mismatch", v("0.157.0"), &sha('A'))
            .unwrap();
        let got = ProgramStamp::read(&l, "codex").unwrap();
        assert!(got.is_refused(v("0.157.0"), &sha('a')));
        assert!(
            !got.is_refused(v("0.157.0"), &sha('b')),
            "a new digest is a new pair"
        );
        assert!(
            !got.is_refused(v("0.157.1"), &sha('a')),
            "a new head is a new pair"
        );
        assert_eq!(got.high_water, Some(v("0.156.0")));
        assert_eq!(got.verdict, "digest mismatch");
        assert!(
            ProgramStamp::record_refusal(&l, "codex", "x", v("0.157.0"), "abc").is_err(),
            "not a sha256"
        );
        // A torn pair on disk is no refusal.
        let path = stamp_path(&l, "codex").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let torn: String = text
            .lines()
            .filter(|line| !line.starts_with("refused_sha256"))
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&path, torn).unwrap();
        assert_eq!(ProgramStamp::read(&l, "codex").unwrap().refused, None);
    }

    #[test]
    fn the_explicit_door_forgets_a_refusal_and_nothing_else() {
        let l = layout("clear");
        ProgramStamp::clear_refusal(&l, "codex").unwrap();
        assert!(!vendor_dir(&l).exists(), "nothing to clear writes nothing");
        ProgramStamp::record_outcome(&l, "codex", "kept", Some(v("0.156.0"))).unwrap();
        ProgramStamp::record_refusal(&l, "codex", "digest mismatch", v("0.157.0"), &sha('a'))
            .unwrap();
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

    /// No terminal escape, bell, C1 control, line separator or bidi override survives into
    /// a line `status` and Settings print.
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
        // A hand-edited file is cleaned on read as well.
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

    /// A legacy build's version, once read, is kept by build number beside every other
    /// field; a vendor build id is never recorded as legacy, and a newer legacy build's
    /// version replaces an older one's.
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

    /// A legacy build's signed root is kept by its build number apart from its version (the
    /// two are learned at different moments), lowercased; a vendor build id or a root that
    /// is not 64 hex digits is refused, and a hand-mangled root reads as none.
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

    /// A key this build does not know — one a newer build writes — survives every rewrite
    /// this build makes. Builds that rewrote the file from their own fields alone dropped
    /// what a newer one kept: the legacy root, to every build before 2026-09-23.
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

    /// A check vouches for "latest" for a day; never checked, or stamped more than an hour
    /// ahead of the clock, vouches for nothing.
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
