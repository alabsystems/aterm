// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
    ProgramStamp::record_outcome(&l, "claude", "installed 2.1.281", Some(v("2.1.281"))).unwrap();
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
        one_line("a\u{1b}]0;title\u{7}b\u{9b}31m\tc\u{2028}d\u{2029}e\u{202e}f\u{2066}g\u{7f}h"),
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
        ProgramStamp::record_legacy_version(&l, "claude", v("2.1.280").build_id(), v("2.1.280"))
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

#[cfg(unix)]
#[test]
fn update_replaces_symlink_even_when_target_bytes_match() {
    let l = layout("symlink-stamp");
    ProgramStamp::record_check(&l, "claude", 7, None, None).unwrap();
    let path = stamp_path(&l, "claude").unwrap();
    let target = l.prefix.join("fake-stamp.toml");
    std::fs::copy(&path, &target).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(ProgramStamp::read(&l, "claude"), None);
    ProgramStamp::record_check(&l, "claude", 7, None, None).unwrap();
    assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
    assert_eq!(
        ProgramStamp::read(&l, "claude"),
        Some(ProgramStamp {
            last_checked_at: 7,
            ..ProgramStamp::default()
        })
    );
}
