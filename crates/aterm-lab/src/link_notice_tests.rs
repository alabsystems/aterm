// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The lines a test's teller kept that name `under`.
fn kept_under(under: &Path) -> Vec<String> {
    let teller = TELLER.get_or_init(|| Some(Teller::Kept(Mutex::new(Vec::new()))));
    let Some(Teller::Kept(lines)) = teller else {
        panic!("a test's teller");
    };
    let under = under.display().to_string();
    lines
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|l| l.contains(&under))
        .cloned()
        .collect()
}

/// THE LAB'S SCRATCH READS TELL EACH LINK THEY REFUSE ONCE: a file read and a listing whose
/// path runs through a planted link, a removal under it and a listing that meets a link in a
/// folder each tell its path, and a second refusal of the same path tells nothing.
/// NEGATIVE CONTROL: drop the scratch file read's report of its refusal, and its path is not
/// told.
#[test]
fn the_scratch_reads_tell_each_link_they_refuse_once() {
    use std::os::unix::fs::symlink;
    let _ = kept_under(Path::new("/"));
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let out = root.join("out");
    std::fs::create_dir(&out).expect("out");
    std::fs::write(out.join("result.json"), b"{}").expect("a result");
    symlink(&out, root.join("planted")).expect("a planted folder link");
    symlink("/etc/hosts", out.join("hosts")).expect("a planted file link");
    let through = root.join("planted/result.json");
    assert!(crate::sys::scratch_file(&through).is_err(), "refused");
    assert!(crate::sys::scratch_file(&through).is_err(), "refused again");
    assert!(crate::sys::scratch_dir(&root.join("planted")).is_err());
    assert!(crate::sys::scratch_remove(&root.join("planted/gone")).is_err());
    let listed = crate::sys::scratch_dir(&out)
        .expect("listed")
        .expect("there");
    assert_eq!(listed.len(), 1, "the link is left out: {listed:?}");
    let told = |p: &Path| {
        format!(
            "lab refused {}: a link in its path, not followed",
            p.display()
        )
    };
    assert_eq!(
        kept_under(&root),
        vec![
            told(&through),
            told(&root.join("planted")),
            told(&root.join("planted/gone")),
            told(&out.join("hosts")),
        ]
    );
}

/// A LAB A TEST STARTED TELLS NOTHING: this test binary is a Cargo test's, so the lab's
/// verbs it runs arm no teller, whatever session it inherits. NEGATIVE CONTROL: read only
/// the parent chain above this process, and this process reads as no test's.
#[test]
fn a_lab_a_test_started_tells_nothing() {
    assert!(started_by_a_test(std::process::id()));
    assert!(!started_by_a_test(1), "launchd");
}

/// A REFUSED LINK IS TOLD ONCE: the first refusal of a path makes its line (the path, and
/// that it was not followed), a second makes none, and another path its own; a path's control
/// characters are spaced and a long one is cut, so the line stays one line the instance takes.
#[test]
fn a_refused_link_is_told_once() {
    let mut told = BTreeSet::new();
    let path = Path::new("/w/runs/lab-1/out/result.json");
    assert_eq!(
        line_once(&mut told, path).as_deref(),
        Some("lab refused /w/runs/lab-1/out/result.json: a link in its path, not followed")
    );
    assert_eq!(line_once(&mut told, path), None, "told once");
    let odd = PathBuf::from(format!("/w/a\nb/{}", "x".repeat(600)));
    let line = line_once(&mut told, &odd).expect("another path");
    assert!(!line.contains('\n') && line.len() < 512, "{line}");
    assert!(line.starts_with("lab refused /w/a b/xxx"), "{line}");
}
