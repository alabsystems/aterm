// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn sized(path: &Path, bytes: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

fn names(survey: &Survey, home: &Path) -> Vec<String> {
    survey
        .holders
        .iter()
        .map(|holder| {
            holder
                .path
                .strip_prefix(home)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// A fixture home: two scopes, a worktree root, a checkout, the shared cache, and folders that
/// are never looked into.
fn home_with_holders(home: &Path) {
    sized(
        &home.join(".cache/alab-release-scope/big/target/a.o"),
        600_000,
    );
    sized(
        &home.join(".cache/alab-release-scope/small/note.json"),
        10_000,
    );
    sized(&home.join(".ubd/root-a/build/b.o"), 300_000);
    sized(&home.join("proj/src/lib.rs"), 150_000);
    sized(
        &crate::build_cache::cache_root(home).join("Users/x/proj/.cargo-build/c.o"),
        80_000,
    );
    #[cfg(target_os = "macos")]
    sized(&home.join("Documents/private.txt"), 900_000);
    sized(
        &home.join("Library/Application Support/other/app.db"),
        900_000,
    );
}

/// The measure names the largest folders first, each with what it is, and keeps them in the
/// ledger; it never names a folder of `$HOME` that is never looked into, nor a place's own
/// root beside its units. NEGATIVE CONTROL: the small scope grown past every other is first in
/// the measure an hour later, so the order is measured and not the listing's; a measure within
/// the hour walks nothing again and keeps the earlier figures.
#[test]
fn the_largest_holders_are_named_first_and_kept_in_the_ledger() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    home_with_holders(&home);
    let now = SystemTime::now();
    let survey = measure(&home, &[], now, Duration::from_secs(60));
    assert_eq!(
        names(&survey, &home),
        [
            ".cache/alab-release-scope/big",
            ".ubd/root-a",
            "proj",
            "Library/Caches/aterm/.ubd/cargo-build.noindex",
            ".cache/alab-release-scope/small",
        ],
        "{survey:#?}"
    );
    assert_eq!(survey.unmeasured, 0);
    let kinds: Vec<&str> = survey.holders.iter().map(|h| h.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "campaign scope",
            "worktree root",
            "folder in $HOME",
            "shared build cache",
            "campaign scope"
        ]
    );
    assert!(
        survey
            .holders
            .iter()
            .all(|h| h.complete && h.written.is_some())
    );
    assert!(survey.holders[0].bytes >= 600_000 && survey.holders[0].bytes < 1_000_000);
    assert!(survey.bytes() >= 1_140_000);
    let mut kept = read(&ledger(&home));
    kept.sort_by_key(|holder| std::cmp::Reverse(holder.bytes));
    assert_eq!(kept, survey.holders, "the ledger keeps the measure");

    sized(
        &home.join(".cache/alab-release-scope/small/target/d.o"),
        2_000_000,
    );
    let soon = measure(
        &home,
        &[],
        now + Duration::from_secs(5),
        Duration::from_secs(60),
    );
    assert_eq!(
        soon, survey,
        "measured whole within the hour: not walked again"
    );
    let later = now + FRESH;
    let again = measure(&home, &[], later, Duration::from_secs(60));
    assert_eq!(names(&again, &home)[0], ".cache/alab-release-scope/small");
    let line = again.holders[0].line(later + Duration::from_secs(60));
    assert!(
        line.contains("campaign scope") && line.contains("measured 60s ago"),
        "{line}"
    );
}

/// A budget is spent on what was never measured, then on what was measured longest ago, and a
/// walk it ends keeps an earlier whole figure or says a lower bound. NEGATIVE CONTROL: with no
/// stop, the same call measures every folder whole.
#[test]
fn a_budget_measures_the_never_measured_first_and_never_loses_a_whole_figure() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (a, b, c) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
    );
    for (path, bytes) in [(&a, 100_000), (&b, 200_000), (&c, 400_000)] {
        sized(&path.join("file"), bytes);
    }
    let ledger = dir.path().join("state/holders.json");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
    let at = |survey: &Survey, path: &Path| {
        survey
            .holders
            .iter()
            .find(|holder| holder.path == path)
            .cloned()
    };
    // The ledger as two earlier calls left it: `a` measured whole at 1000, `b` at 2000.
    let whole_at = |path: &Path, bytes: u64, measured: u64| Holder {
        path: path.to_path_buf(),
        kind: "folder".to_owned(),
        bytes,
        complete: true,
        measured,
        written: None,
    };
    write(
        &ledger,
        &[whole_at(&a, 111, 1_000), whole_at(&b, 222, 2_000)],
    );
    let hour = |n: u64| t0 + Duration::from_secs(n * 3_600);
    let stamp = |n: u64| 100_000 + n * 3_600;

    // One walk's worth of budget: the first check lets a walk begin, every later one ends it.
    let all = [
        (a.clone(), "folder"),
        (b.clone(), "folder"),
        (c.clone(), "folder"),
    ];
    let calls = std::cell::Cell::new(0u32);
    let one_check = || {
        calls.set(calls.get() + 1);
        calls.get() > 1
    };
    let cut = measure_in(&ledger, &all, hour(1), &one_check);
    let c_cut = at(&cut, &c).expect("the never measured folder is walked first");
    assert!(!c_cut.complete, "its walk was ended: a lower bound");
    assert!(c_cut.bytes < 400_000);
    assert_eq!(c_cut.measured, stamp(1));
    for (path, when, bytes) in [(&a, 1_000, 111), (&b, 2_000, 222)] {
        let kept = at(&cut, path).unwrap();
        assert!(
            kept.complete && kept.measured == when && kept.bytes == bytes,
            "{kept:?}"
        );
    }

    // The same budget again: the lower bound is the oldest whole figure's turn no longer, so
    // `a`, measured longest ago, is next; its walk is ended and its whole figure stands.
    calls.set(0);
    let again = measure_in(&ledger, &all, hour(2), &one_check);
    assert_eq!(at(&again, &a), Some(whole_at(&a, 111, 1_000)));
    assert_eq!(
        at(&again, &c).unwrap().measured,
        stamp(1),
        "not walked again"
    );

    let whole = measure_in(&ledger, &all, hour(3), &|| false);
    assert!(whole.holders.iter().all(|holder| holder.complete));
    assert_eq!(whole.holders[0].path, c);
    assert!(
        whole
            .holders
            .iter()
            .all(|holder| holder.measured == stamp(3))
    );

    // A folder that is no candidate any more leaves the ledger.
    let fewer = measure_in(&ledger, &all[..2], hour(4), &|| false);
    assert!(at(&fewer, &c).is_none());
    assert_eq!(read(&ledger).len(), 2);
}
