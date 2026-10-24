// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lineage receipt switch tests over local bare git origins.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use atpkg_publish::artifact::lineage_switch::{self, Switch};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::testkit::Keys;

use super::fixture::{
    DORMANT_LEDGER, MINTED_LEDGER, git, push_from_elsewhere, scratch, trust_repo,
};

fn ctx(home: &Path) -> Ctx {
    Keys::mint("m3", &[]).ctx(home, Arc::new(FakeHost::new()))
}

fn read(home: &Path, repo: &Path) -> Switch {
    lineage_switch::read(&ctx(home), repo).expect("the switch always answers")
}

fn unknown(s: &Switch) -> &str {
    match s {
        Switch::Unknown(why) => why,
        other => panic!("expected Unknown, got {other:?}"),
    }
}

fn origin_main(repo: &Path) -> String {
    git(repo, &["rev-parse", "refs/remotes/origin/main"])
}

fn objects(repo: &Path) -> std::path::PathBuf {
    repo.join(".git/objects")
}

fn quarantines(repo: &Path) -> Vec<String> {
    std::fs::read_dir(objects(repo))
        .expect("objects")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("atpkg-lineage-fetch."))
        .collect()
}

#[test]
fn a_ledger_with_no_mint_at_the_live_main_is_dormant_and_a_minted_one_is_live() {
    let tmp = scratch("switch-basic");
    let dormant = tmp.path().join("dormant");
    trust_repo(&dormant, DORMANT_LEDGER);
    assert_eq!(read(tmp.path(), &dormant), Switch::Dormant);
    let live = tmp.path().join("live");
    trust_repo(&live, MINTED_LEDGER);
    assert_eq!(read(tmp.path(), &live), Switch::Live);
    assert_eq!(
        lineage_switch::default_trust_repo(&ctx(tmp.path())),
        tmp.path().join("trust")
    );
}

#[test]
fn a_mint_that_landed_after_this_box_last_fetched_makes_it_live_and_moves_no_ref() {
    let tmp = scratch("switch-stale");
    let repo = tmp.path().join("stale");
    let bare = trust_repo(&repo, DORMANT_LEDGER);
    let before = origin_main(&repo);
    let fetch_head = std::fs::read(repo.join(".git/FETCH_HEAD")).expect("the fixture fetched");
    push_from_elsewhere(&bare, tmp.path(), Some(MINTED_LEDGER));
    let pushed = git(&bare, &["rev-parse", "refs/heads/main"]);

    assert_eq!(read(tmp.path(), &repo), Switch::Live);
    assert_eq!(
        origin_main(&repo),
        before,
        "the person's origin/main reads as it did"
    );
    assert_eq!(
        std::fs::read(repo.join(".git/FETCH_HEAD")).expect("still there"),
        fetch_head,
        "no FETCH_HEAD is written"
    );
    git(&repo, &["cat-file", "-e", &format!("{pushed}^{{commit}}")]);
    assert_eq!(
        std::fs::read_to_string(repo.join(".git/atpkg-lineage-live"))
            .expect("the live main is noted for the next negotiation")
            .trim(),
        pushed
    );
    assert!(quarantines(&repo).is_empty(), "{:?}", quarantines(&repo));
}

#[test]
fn an_unrelated_push_keeps_it_dormant_read_at_the_live_main() {
    let tmp = scratch("switch-behind");
    let repo = tmp.path().join("behind");
    let bare = trust_repo(&repo, DORMANT_LEDGER);
    push_from_elsewhere(&bare, tmp.path(), None);
    assert_eq!(read(tmp.path(), &repo), Switch::Dormant);
    assert_eq!(read(tmp.path(), &repo), Switch::Dormant);
}

#[test]
fn every_way_of_not_knowing_is_unknown_never_dormant() {
    let tmp = scratch("switch-unknown");
    let home = tmp.path();

    let unreachable = home.join("unreachable");
    trust_repo(&unreachable, DORMANT_LEDGER);
    git(
        &unreachable,
        &[
            "remote",
            "set-url",
            "origin",
            &home.join("no-such-origin.git").to_string_lossy(),
        ],
    );
    let why = read(home, &unreachable);
    assert!(
        unknown(&why).contains("`git ls-remote origin` failed"),
        "{why:?}"
    );
    assert!(unknown(&why).contains("fetch origin main"), "{why:?}");

    let empty_origin = home.join("empty-origin");
    trust_repo(&empty_origin, DORMANT_LEDGER);
    let bare = home.join("empty.git");
    std::fs::create_dir_all(&bare).expect("mkdir");
    git(&bare, &["init", "-q", "--bare"]);
    git(
        &empty_origin,
        &["remote", "set-url", "origin", &bare.to_string_lossy()],
    );
    let why = read(home, &empty_origin);
    assert!(unknown(&why).contains("its origin has no main"), "{why:?}");

    let why = read(home, &home.join("no-checkout"));
    assert!(unknown(&why).contains("no trust checkout"), "{why:?}");
    let why = read(home, Path::new(""));
    assert!(unknown(&why).contains("no trust checkout"), "{why:?}");

    let no_origin = home.join("no-origin");
    std::fs::create_dir_all(&no_origin).expect("mkdir");
    git(&no_origin, &["init", "-q", "."]);
    let why = read(home, &no_origin);
    assert!(unknown(&why).contains("has no origin/main"), "{why:?}");

    let no_ledger = home.join("no-ledger");
    trust_repo(&no_ledger, DORMANT_LEDGER);
    git(&no_ledger, &["rm", "-q", lineage_switch::LEDGER]);
    git(&no_ledger, &["commit", "-q", "-m", "drop the ledger"]);
    git(
        &no_ledger,
        &["push", "-q", "origin", "HEAD:refs/heads/main"],
    );
    git(&no_ledger, &["fetch", "-q", "origin"]);
    let why = read(home, &no_ledger);
    assert!(unknown(&why).contains("no readable"), "{why:?}");
}

#[test]
fn a_failed_fetch_is_unknown_and_leaves_no_quarantine_behind() {
    let tmp = scratch("switch-failed-fetch");
    let repo = tmp.path().join("broken");
    let bare = trust_repo(&repo, DORMANT_LEDGER);
    push_from_elsewhere(&bare, tmp.path(), None);
    let pushed = git(&bare, &["rev-parse", "refs/heads/main"]);
    let loose = bare.join("objects").join(&pushed[..2]).join(&pushed[2..]);
    std::fs::remove_file(&loose).expect("the pushed commit is a loose object in the bare origin");

    let why = read(tmp.path(), &repo);
    assert!(unknown(&why).contains("fetching it failed"), "{why:?}");
    assert!(quarantines(&repo).is_empty(), "{:?}", quarantines(&repo));
}

#[test]
fn a_quarantine_older_than_an_hour_is_swept_and_a_fresh_one_kept() {
    let tmp = scratch("switch-sweep");
    let repo = tmp.path().join("sweep");
    let bare = trust_repo(&repo, DORMANT_LEDGER);
    let old = objects(&repo).join("atpkg-lineage-fetch.killed");
    let fresh = objects(&repo).join("atpkg-lineage-fetch.concurrent");
    for d in [&old, &fresh] {
        std::fs::create_dir_all(d.join("pack")).expect("mkdir quarantine");
    }
    std::fs::File::open(&old)
        .expect("open dir")
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 3600))
        .expect("age the quarantine");
    push_from_elsewhere(&bare, tmp.path(), None);
    assert_eq!(read(tmp.path(), &repo), Switch::Dormant);
    assert_eq!(
        quarantines(&repo),
        vec!["atpkg-lineage-fetch.concurrent".to_string()]
    );
}

#[test]
fn only_a_dormant_reading_admits_an_unproven_compiler() {
    use atpkg::lineage::LineagePolicy;
    assert_eq!(Switch::Dormant.policy(), LineagePolicy::Dormant);
    assert_eq!(Switch::Live.policy(), LineagePolicy::Require);
    assert_eq!(Switch::Unknown("x".into()).policy(), LineagePolicy::Require);
}
