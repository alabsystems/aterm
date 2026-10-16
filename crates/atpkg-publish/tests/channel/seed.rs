// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SEED PIN at publish time (`channel::seed::publish_refusals`) against a real trust
//! origin in scratch git: publishing waits until origin main contains this cut's re-pin,
//! pins this compiler through it (and the generated section byte for byte), its ledger
//! names the re-pin's archive release, and that release exists.

use std::path::{Path, PathBuf};

use atpkg_publish::channel::seed::{self, GENERATED_MARKER, publish_refusals};
use atpkg_publish::host::Slug;

use super::{Rig, git};

const SEAL_COMMIT: &str = "1234567890123456789012345678901234567890";
const TAG: &str = "trust-stage0-2026-09-29-fixture";

fn stage0(compiler: &str, archives: &str) -> String {
    format!(
        "# the trust repo's stage0 pin\ncompiler_git_commit_hash={compiler}\n{GENERATED_MARKER}\n\
         dist/{archives}.tar.xz=abc\n"
    )
}

fn ledger(tag: &str) -> String {
    format!("[release]\nrepo = \"owner/trust\"\ntag = \"{tag}\"\n")
}

fn commit(dir: &Path, stage0_text: &str, ledger_text: &str, msg: &str) -> String {
    std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
    std::fs::create_dir_all(dir.join("bootstrap/trust-stage0")).expect("mkdir ledger");
    std::fs::write(dir.join("src/stage0"), stage0_text).expect("stage0");
    std::fs::write(dir.join(seed::LEDGER), ledger_text).expect("ledger");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

/// An origin whose main carries the re-pin, a consumer clone of it, and a seal whose
/// SEED-MINT names that re-pin.
struct Fixture {
    rig: Rig,
    origin: PathBuf,
    consumer: PathBuf,
    seal: PathBuf,
    repin: String,
}

fn fixture(label: &str) -> Fixture {
    fixture_with(label, true)
}

fn fixture_with(label: &str, archives_published: bool) -> Fixture {
    let rig = Rig::new(label);
    let origin = rig.path("trust-origin");
    std::fs::create_dir_all(&origin).expect("mkdir");
    git(&origin, &["init", "-q", "-b", "main"]);
    commit(
        &origin,
        &stage0("0000", "old"),
        &ledger("trust-stage0-old"),
        "pin old",
    );
    let repin = commit(
        &origin,
        &stage0(SEAL_COMMIT, "minted"),
        &ledger(TAG),
        "re-pin",
    );
    let consumer = rig.path("trust");
    git(
        rig.tmp.path(),
        &[
            "clone",
            "-q",
            &origin.display().to_string(),
            &consumer.display().to_string(),
        ],
    );
    let seal = rig.path("seal");
    std::fs::create_dir_all(&seal).expect("seal");
    write_receipt(&seal, &repin);
    if archives_published {
        rig.host.put_asset(
            &Slug::new("owner", "trust"),
            TAG,
            "stage0.tar.xz",
            b"x".to_vec(),
        );
    }
    Fixture {
        rig,
        origin,
        consumer,
        seal,
        repin,
    }
}

fn write_receipt(seal: &Path, repin: &str) {
    std::fs::write(
        seal.join(atpkg::lineage::SEED_MINT_FILE),
        format!(
            "repin_commit={repin}\nrepin_release_tag={TAG}\nrepin_compiler_git_commit_hash={SEAL_COMMIT}\nrepin_branch=seed/repin-fixture\n"
        ),
    )
    .expect("receipt");
}

fn refusals(f: &Fixture) -> Vec<String> {
    publish_refusals(&f.rig.ctx, &f.consumer, &f.seal)
}

#[test]
fn a_repin_main_carries_with_its_release_publishes() {
    let f = fixture("seed-ok");
    assert_eq!(refusals(&f), Vec::<String>::new());
}

#[test]
fn the_archives_must_be_published() {
    let f = fixture_with("seed-norelease", false);
    let r = refusals(&f);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(
        r[0].contains(&format!(
            "the seed archives are not published: release {TAG} on owner/trust does not exist"
        )),
        "{r:?}"
    );
}

#[test]
fn main_must_contain_this_cuts_repin() {
    let f = fixture("seed-otherrepin");
    git(&f.origin, &["checkout", "-q", "-b", "side"]);
    let other = commit(
        &f.origin,
        &stage0(SEAL_COMMIT, "elsewhere"),
        &ledger(TAG),
        "side re-pin",
    );
    git(&f.origin, &["checkout", "-q", "main"]);
    git(&f.consumer, &["fetch", "-q", "origin", "side"]);
    write_receipt(&f.seal, &other);
    let r = refusals(&f);
    assert!(
        r.len() == 1
            && r[0].contains(&format!(
                "does not contain this cut's re-pin commit {other}"
            )),
        "{r:?}"
    );
}

#[test]
fn main_must_pin_this_compiler_through_the_repins_archives() {
    let f = fixture("seed-superseded");
    commit(
        &f.origin,
        &stage0("5555555555", "later"),
        &ledger(TAG),
        "a later re-pin",
    );
    let r = refusals(&f);
    assert!(
        r.len() == 1
            && r[0].contains("but pins 5555555555 in src/stage0, not this seal's compiler"),
        "{r:?}"
    );

    let f = fixture("seed-generated");
    commit(
        &f.origin,
        &stage0(SEAL_COMMIT, "other-archives"),
        &ledger(TAG),
        "re-archive",
    );
    let r = refusals(&f);
    let short: String = f.repin.chars().take(10).collect();
    assert!(
        r.len() == 1
            && r[0].contains(&format!(
                "the generated section of src/stage0 on {}'s origin main is not the re-pin {short}'s",
                f.consumer.display()
            )),
        "{r:?}"
    );
}

#[test]
fn the_ledger_must_name_this_cuts_release() {
    let f = fixture("seed-ledger");
    commit(
        &f.origin,
        &stage0(SEAL_COMMIT, "minted"),
        &ledger("trust-stage0-2026-09-30-other"),
        "another ledger",
    );
    let r = refusals(&f);
    assert!(
        r.len() == 1
            && r[0].contains(&format!(
                "names release 'owner/trust trust-stage0-2026-09-30-other', not this cut's {TAG}"
            )),
        "{r:?}"
    );
}

#[test]
fn no_origin_is_a_refusal_never_a_pass() {
    let f = fixture("seed-noorigin");
    git(&f.consumer, &["remote", "remove", "origin"]);
    let r = refusals(&f);
    assert!(
        r.len() == 1
            && r[0].contains(&format!(
                "cannot fetch {}'s origin main",
                f.consumer.display()
            )),
        "{r:?}"
    );
}

#[test]
fn the_receipt_is_described_honestly() {
    let receipt = "pinned_seed_commit=1111111111aaaa\nddc_seed_commit=2222222222bbbb\n\
                   ddc_seed_lineage=trust\nddc_seed_relation=pin-ancestor\n\
                   stage2_tree_sha256_pinned_seed=feed\nlineage_disjoint_anchor=none\n\
                   repin_commit=3333333333cccc\nrepin_branch=seed/repin-x\nrepin_release_tag=t\n";
    let line = seed::describe(receipt);
    assert_eq!(
        line,
        "built from the pin 1111111111; double compile against 2222222222 (trust lineage, \
         pin-ancestor: not diverse): both stage2 trees feed; latest lineage-disjoint cut none; \
         re-pin 3333333333 (seed/repin-x), archives t"
    );
    assert!(
        seed::describe(&receipt.replace("pin-ancestor", "lineage-disjoint"))
            .contains("lineage-disjoint: diverse")
    );
}
