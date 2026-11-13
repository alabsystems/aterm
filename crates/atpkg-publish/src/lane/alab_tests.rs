// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn a_compiler_that_fails_its_version_print_names_no_pin_commit() {
    use std::os::unix::process::ExitStatusExt as _;
    use std::sync::Arc;

    use crate::host::{Hosts, fake::FakeHost};

    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let mut ctx = Ctx::new(
        tmp.path(),
        None,
        None,
        0,
        Hosts::single(Arc::new(FakeHost::new())),
    );
    ctx.pkg_prefix = tmp.path().join("prefix");
    let compiler = ctx.pkg_prefix.join("store/trust/8/bin/trustc");
    for success in [None, Some(false), Some(true)] {
        let result = installed_pin_commit_with(&ctx, "trust", 8, |cmd| {
            assert_eq!(cmd.get_program(), compiler.as_os_str());
            assert_eq!(
                cmd.get_args().collect::<Vec<_>>(),
                [std::ffi::OsStr::new("-vV")]
            );
            success.map(|success| std::process::Output {
                status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 }),
                stdout: b"commit-hash: 321aaeda7f00ba\ncommit-date: 2026-09-17\n".to_vec(),
                stderr: Vec::new(),
            })
        });
        assert_eq!(
            result,
            (success == Some(true)).then(|| PinCommit {
                commit: "321aaeda7".into(),
                date: Some("2026-09-17".into()),
            }),
            "observed success: {success:?}"
        );
    }
}

/// THE RUN'S BARE REPOSITORY IS ONE AGH READS UNDER ANY UMASK: `git init` under umask 002
/// leaves engine.git 0775 and its config 0664, whose group write bit agh refuses on every
/// fetch of the engine's main. NEGATIVE CONTROL: without [`owner_written`] the read is
/// refused as writable.
#[cfg(unix)]
#[test]
fn the_engine_repository_is_one_agh_reads_under_a_group_umask() {
    use std::os::unix::fs::PermissionsExt as _;
    let work = aterm_tempfile::Builder::new()
        .prefix("atpkg-publish-engine-git-")
        .tempdir()
        .expect("scratch dir");
    crate::git::run(work.path(), &["init", "--quiet", "--bare", "engine.git"]).expect("init");
    let repo = work.path().join("engine.git");
    for (path, mode) in [(repo.clone(), 0o775), (repo.join("config"), 0o664)] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
    owner_written(&repo).expect("the write bits cleared");
    let read = aterm_uds::private_file::read_owned(&repo, "config", 1 << 16);
    assert!(read.is_ok(), "{:?}", read.err());
    let mode = |path: &Path| std::fs::metadata(path).expect("stat").permissions().mode();
    assert_eq!(
        (mode(&repo) & 0o7777, mode(&repo.join("config")) & 0o7777),
        (0o755, 0o644)
    );
}

fn ledger(one: &str, two: &str, live: &str, revoked: &str) -> aterm_json::Value {
    aterm_json::from_str::<aterm_json::Value>(&format!(
        "{{\"mappings\":{{\"ay\":{{\"{one}\":{{\"release_rev\":\"d766\"}},\
         \"{two}\":{{\"release_rev\":\"d766\"}},\"{live}\":{{\"release_rev\":\"d766\"}},\
         \"{}\":{{\"release_rev\":\"other\"}}}}}},\"revoked\":{{{revoked}}}}}",
        "9".repeat(40)
    ))
    .unwrap()
}

/// A REVOKED ROW NEVER NAMES THE DEV COMMIT A SIGNED RELEASE WAS CUT FROM, AND TWO LIVE ROWS
/// DECIDE NONE: ay's signed commit d766… is the release of two revoked rows and one live one,
/// as the ledger held it on 2026-11-07, and only the live one is its source. NEGATIVE
/// CONTROL: with the revocations dropped, the sorted first row (a revoked one) was taken.
#[test]
fn a_revoked_row_never_names_the_source_and_two_live_rows_decide_none() {
    let (one, two, live) = ("0".repeat(40), "5".repeat(40), "6".repeat(40));
    let revoked = ledger(
        &one,
        &two,
        &live,
        &format!("\"ay\":{{\"{one}\":{{}},\"{two}\":{{}}}}"),
    );
    assert_eq!(
        sources(&revoked, "ay", "d766"),
        (vec![live.as_str()], vec![one.as_str(), two.as_str()])
    );
    assert_eq!(sources(&revoked, "trust-ir", "d766"), (vec![], vec![]));
    let unrevoked = ledger(&one, &two, &live, "");
    assert_eq!(
        sources(&unrevoked, "ay", "d766").0,
        vec![one.as_str(), two.as_str(), live.as_str()],
        "NEGATIVE CONTROL: without the revocations every row is live, so none is decided"
    );
}

/// A RELEASE WHOSE EVERY ROW IS REVOKED IS SAID TO BE WITHDRAWN, NOT AWAITING ITS PROMOTE:
/// the engine refuses to map a revoked source again, so waiting for a promote would wait for
/// one that already ran. NEGATIVE CONTROL: the same rows unrevoked are no withdrawal, and a
/// release with no row at all is still awaiting its promote.
#[test]
fn a_release_whose_every_row_is_revoked_is_withdrawn_not_awaited() {
    let (one, two, three) = ("0".repeat(40), "5".repeat(40), "6".repeat(40));
    let every = ledger(
        &one,
        &two,
        &three,
        &format!("\"ay\":{{\"{one}\":{{}},\"{two}\":{{}},\"{three}\":{{}}}}"),
    );
    let parsed = |value| CachedLedger::Parsed {
        at: "engine main 0123".into(),
        value,
    };
    let clone = Path::new("/nonexistent");
    let why = resolve_dev(&parsed(every.clone()), "ay", "d766", clone).unwrap_err();
    assert!(why.contains("every row naming it is revoked"), "{why}");
    assert!(
        why.contains(&one[..12]) && why.contains(&three[..12]),
        "{why}"
    );
    assert!(!why.contains("recorded none"), "{why}");
    let none = resolve_dev(&parsed(every), "ay", "absent", clone).unwrap_err();
    assert!(
        none.contains("its promote has recorded none there yet"),
        "{none}"
    );
    let live =
        resolve_dev(&parsed(ledger(&one, &two, &three, "")), "ay", "d766", clone).unwrap_err();
    assert!(
        live.contains("3 live ay rows") && !live.contains("withdrew"),
        "NEGATIVE CONTROL: {live}"
    );
}
