// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const FULL: &str = "1a37de31a3401234567890abcdef1234567890ab";

fn build() -> Build {
    Build {
        version: "0.199.0".to_string(),
        commit: FULL[..12].to_string(),
        commit_full: FULL.to_string(),
        build_number: 1_794_421_328,
        arch: "x64".to_string(),
        toolchain: "stock 1.97.1".to_string(),
        dirty: false,
        built_at: "2026-11-11T19:14:22Z".to_string(),
    }
}

fn pointer() -> Pointer {
    // An absolute path on the host the test runs on, with a separator to escape.
    let dir = std::env::temp_dir()
        .join("aterm")
        .join("dist")
        .join(&FULL[..12]);
    Pointer::of(
        &build(),
        &dir,
        "2026-11-11T19:20:00Z".to_string(),
        Smoke::Pass,
    )
}

#[test]
fn build_json_parses_as_build_ps1_writes_it() {
    let text = format!(
        "\u{feff}{{\"schema\":1,\"version\":\"0.199.0\",\"commit\":\"{}\",\"commit_full\":\"{FULL}\",\
         \"build_number\":1794421328,\"arch\":\"x64\",\"toolchain\":\"stock 1.97.1\",\"dirty\":false,\
         \"built_at\":\"2026-11-11T19:14:22Z\"}}\r\n",
        &FULL[..12]
    );
    assert_eq!(Build::parse(&text), Ok(build()));
    // Negative controls: one field wrong at a time.
    let bad = |from: &str, to: &str| Build::parse(&text.replace(from, to));
    assert!(
        bad("\"schema\":1", "\"schema\":2").is_err(),
        "a newer schema"
    );
    assert!(
        bad(
            "\"build_number\":1794421328",
            "\"build_number\":\"1794421328\""
        )
        .is_err()
    );
    let short = format!("\"commit\":\"{}\"", &FULL[..12]);
    assert!(
        bad(&short, "\"commit\":\"1a37de31a399\"").is_err(),
        "commit is not commit_full's prefix"
    );
    assert!(bad("\"dirty\":false,", "").is_err(), "no dirty");
}

#[test]
fn pointer_round_trips_through_its_json() {
    let p = pointer();
    let text = p.to_json();
    assert!(
        text.starts_with("{\"schema\":1,\"version\":\"0.199.0\",\"commit\":"),
        "{text}"
    );
    assert!(text.ends_with("\"smoke\":\"pass\"}\n"), "{text}");
    assert_eq!(Pointer::parse(&text), Ok(p.clone()));
    for smoke in [Smoke::Fail, Smoke::NotRun] {
        let q = Pointer { smoke, ..p.clone() };
        assert_eq!(Pointer::parse(&q.to_json()), Ok(q));
    }
}

#[test]
fn pointer_refuses_what_it_cannot_trust() {
    let text = pointer().to_json();
    assert!(Pointer::parse(&text).is_ok(), "the control parses");
    let bad = |from: &str, to: &str| {
        let changed = text.replace(from, to);
        assert_ne!(changed, text, "{from:?} is in the pointer");
        Pointer::parse(&changed)
    };
    assert!(
        bad("\"schema\":1", "\"schema\":0").is_err(),
        "an older schema"
    );
    assert!(bad("\"schema\":1,", "").is_err(), "no schema");
    assert!(bad("\"smoke\":\"pass\"", "\"smoke\":\"maybe\"").is_err());
    assert!(bad(FULL, &FULL.to_uppercase()).is_err(), "upper-case hex");
    assert!(bad(FULL, &FULL[..39]).is_err(), "a short commit_full");
    assert!(bad("\"build_number\":1794421328", "\"build_number\":-1").is_err());
    let dir = aterm_json::quoted_string(&pointer().dir.to_string_lossy())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .expect("a path quotes");
    assert!(bad(&dir, "\"dist\"").is_err(), "a relative dir");
    assert!(Pointer::parse("").is_err());
    assert!(
        Pointer::parse(&text[..text.len() / 2]).is_err(),
        "a torn file"
    );
}

#[test]
fn pointer_write_replaces_the_file_whole() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let path = scratch.path().join("dist").join("latest.json");
    assert_eq!(
        Pointer::read(&path),
        Ok(None),
        "no pointer yet is not an error"
    );
    let first = pointer();
    first.write(&path).expect("first write");
    assert_eq!(Pointer::read(&path), Ok(Some(first.clone())));
    let second = Pointer {
        smoke: Smoke::Fail,
        staged_at: "2026-11-12T03:30:00Z".to_string(),
        ..first
    };
    second.write(&path).expect("second write");
    assert_eq!(Pointer::read(&path), Ok(Some(second)));
    // Only the pointer is left beside it: the temporary was renamed, not copied.
    let names: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("latest.json")]);
    // Negative control: a file that is not a pointer is an error, never `None`.
    std::fs::write(&path, b"{}").expect("write");
    assert!(Pointer::read(&path).is_err());
}

#[test]
fn rfc3339_names_known_instants() {
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339(1_794_421_328), "2026-11-11T18:22:08Z");
    assert_eq!(rfc3339(4_102_444_799), "2099-12-31T23:59:59Z");
    assert!(is_commit12("1a37de31a340"));
    assert!(!is_commit12("1a37de31a34"));
    assert!(!is_commit12("1A37DE31A340"));
    assert!(!is_commit12("latest.json1"));
}
