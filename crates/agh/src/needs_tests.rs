// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

/// A scratch root.
fn scratch() -> aterm_tempfile::TempDir {
    crate::test_fixtures::scratch_dir("needs")
}

/// The mode bits of `path`.
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn needs_are_written_whole_and_listed() {
    let root = scratch();
    let dirs = Dirs::under(root.path());
    let b = Need::new("agh.b", "what b", "why b", "then b", 2);
    let a = Need::new("agh.a", "what a", "why a", "then a", 1);
    raise(&dirs, &b).expect("b");
    raise(&dirs, &a).expect("a");
    assert_eq!(list(&dirs).expect("list"), [a.clone(), b.clone()]);

    assert_eq!(mode(&dirs.needs()), 0o700);
    for (name, text) in [
        (
            "agh.a.json",
            r#"{"at_secs":1,"key":"agh.a","then":"then a","v":1,"what":"what a","why":"why a"}"#,
        ),
        (
            "agh.b.json",
            r#"{"at_secs":2,"key":"agh.b","then":"then b","v":1,"what":"what b","why":"why b"}"#,
        ),
    ] {
        let file = dirs.needs().join(name);
        assert_eq!(mode(&file), 0o600, "{name}");
        assert_eq!(std::fs::read_to_string(&file).expect("the need"), text);
    }

    let again = Need::new("agh.a", "what a2", "why a2", "then a2", 3);
    raise(&dirs, &again).expect("again");
    assert_eq!(list(&dirs).expect("list"), [again, b.clone()]);

    aterm_uds::private_file::write_private(&dirs.needs(), "agh.v2.json", br#"{"v":2}"#)
        .expect("a v2 file");
    let long = format!(
        r#"{{"at_secs":1,"key":"agh.long","then":"t","v":1,"what":"{}","why":"y"}}"#,
        "w".repeat(NEED_MAX)
    );
    aterm_uds::private_file::write_private(&dirs.needs(), "agh.long.json", long.as_bytes())
        .expect("a long file");
    assert_eq!(list(&dirs).expect("list").len(), 2);

    assert!(raised(&dirs, "agh.a"));
    assert!(clear(&dirs, "agh.a").expect("clear"));
    assert!(!raised(&dirs, "agh.a"));
    assert!(!clear(&dirs, "agh.a").expect("clear again"));
    assert!(!raised(&dirs, "agh.UPPER"));

    for key in ["needs.x", "agh.UPPER", "agh. space"] {
        let e = raise(&dirs, &Need::new(key, "w", "y", "t", 1)).expect_err(key);
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{key}");
    }
    let shaped = format!("agh.{}", agh_fake::token("7"));
    let e = raise(&dirs, &Need::new(&shaped, "w", "y", "t", 1)).expect_err("shaped");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!e.to_string().contains(&shaped));
    assert!(!dirs.needs().join(format!("{shaped}.json")).exists());

    let token = agh_fake::token("7");
    let leaky = Need::new("agh.leak", &format!("saw {token}"), "y", "t", 1);
    raise(&dirs, &leaky).expect("leak");
    let text = std::fs::read_to_string(dirs.needs().join("agh.leak.json")).expect("the need");
    assert!(
        text.contains("saw [redacted]") && !text.contains(&token),
        "{text}"
    );
}

#[test]
fn an_over_long_need_is_cut_to_a_file_list_reads() {
    let root = scratch();
    let dirs = Dirs::under(root.path());
    let token = agh_fake::token("7");
    // Plain bytes, two-byte characters, and control bytes JSON writes as six each.
    let what = "w".repeat(9000);
    let why = format!("{token} {}", "é".repeat(5000));
    let then = "\u{1}".repeat(3000);
    let need = Need::new("agh.long", &what, &why, &then, 4);
    raise(&dirs, &need).expect("raised");
    let file = dirs.needs().join("agh.long.json");
    let len = std::fs::read(&file).expect("the need").len();
    assert!(len <= NEED_MAX, "{len}");
    let listed = list(&dirs).expect("list");
    let [got] = listed.as_slice() else {
        panic!("one need: {listed:?}");
    };
    assert_eq!((got.key.as_str(), got.at_secs), ("agh.long", 4));
    for (cut, whole) in [(&got.what, &what), (&got.why, &why), (&got.then, &then)] {
        let head = cut.strip_suffix('…').unwrap_or(cut);
        assert!(!head.is_empty() && head.len() < whole.len(), "{cut:?}");
    }
    assert!(got.what.starts_with('w'));
    assert!(!got.why.contains(&token) && got.why.starts_with("[redacted]"));
    assert!(raised(&dirs, "agh.long"));
}

#[test]
fn a_need_is_raised_into_a_fresh_state_directory() {
    let root = scratch();
    let dirs = Dirs::under(&root.path().join("fresh"));
    assert!(!dirs.state.exists());
    assert!(dirs.state.parent().is_some_and(|p| !p.exists()));
    let need = Need::new("agh.fresh", "w", "y", "t", 1);
    raise(&dirs, &need).expect("raised");
    assert_eq!(list(&dirs).expect("list"), [need]);
    assert_eq!(mode(&dirs.state), 0o700);
    assert_eq!(mode(&dirs.needs()), 0o700);
    assert_eq!(mode(&dirs.needs().join("agh.fresh.json")), 0o600);
}
