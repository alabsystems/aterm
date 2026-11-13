// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use super::*;
use crate::sig::testkit;

const LINUX: &str = "aarch64-unknown-linux-gnu";
const MAC: &str = "aarch64-apple-darwin";
const SOURCE: &str = "fixture:serving";
const TREE: &str = "4646464646464646464646464646464646464646464646464646464646464646";

fn source(_: &str, _: &str) -> Option<String> {
    Some(SOURCE.to_string())
}

/// A signed index naming ay, the rustc tuple (trust and trust-ir), ty, a program it pins
/// nowhere, and claude's vendor row.
fn index() -> TrustedIndex {
    let body = format!(
        "schema = 2\nindex_build = 9\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"{}\"\nroster_seq = {}\n\
         [programs.ay]\nrepo = \"ay\"\n\
         [programs.trust]\nrepo = \"trust\"\ncoherence_group = \"rustc\"\n\
         unavailable_hint = \"Trust is published for ARM macOS only\"\n\
         [programs.trust-ir]\nrepo = \"trust-ir\"\ncoherence_group = \"rustc\"\n\
         [programs.ty]\nrepo = \"ty\"\n\
         [programs.unpinned]\nrepo = \"unpinned\"\n\
         [programs.claude]\nrepo = \"claude\"\n\
         [[channels]]\nname = \"stable\"\n\
         pin = {{ ay = 5, trust = 16387, trust-ir = 7, ty = 9, claude = 2026102001 }}\n",
        testkit::MACHINE_ID,
        testkit::SEQ,
    );
    testkit::trusted_index_at(testkit::SEQ, body.into_bytes())
}

/// `program`'s build `build` with one `[[artifact]]` row per target in `targets`.
fn manifest(program: &str, build: u64, targets: &[&str]) -> Vec<u8> {
    let mut body =
        format!("schema = 2\nprogram = \"{program}\"\nversion = \"1\"\nbuild_number = {build}\n");
    for target in targets {
        body.push_str(&format!(
            "[[artifact]]\ntarget = \"{target}\"\nkind = \"binary\"\n\
             asset = \"{program}-{build}-{target}.tar.zst\"\nsha256 = \"{}\"\n\
             tree_root = \"{TREE}\"\nsize = 1\n",
            "a".repeat(64)
        ));
    }
    body.into_bytes()
}

fn keep(layout: &Layout, program: &str, build: u64, raw: &[u8], seed: &[u8; 32]) {
    let (document_cap, signature_cap) = caps();
    SignedArtifactCache::for_layout(layout)
        .store_verified(
            &SignedArtifactKey::package(SOURCE, program, program, build),
            raw,
            &testkit::sign(seed, raw),
            document_cap,
            signature_cap,
        )
        .expect("the scratch cache keeps the pair");
}

/// The store after a pass learned ay (Linux and macOS rows), trust (macOS only) and trust-ir
/// (Linux only); ty's manifest was never read.
fn learned_store(label: &str) -> crate::testdir::ScratchLayout {
    let layout = crate::testdir::layout(&format!("atpkg-serving-{label}-"));
    keep(
        &layout,
        "ay",
        5,
        &manifest("ay", 5, &[LINUX, MAC]),
        &testkit::MACHINE_SEED,
    );
    keep(
        &layout,
        "trust",
        16387,
        &manifest("trust", 16387, &[MAC]),
        &testkit::MACHINE_SEED,
    );
    keep(
        &layout,
        "trust-ir",
        7,
        &manifest("trust-ir", 7, &[LINUX]),
        &testkit::MACHINE_SEED,
    );
    layout
}

/// THE SIGNED MANIFESTS DECIDE WHAT SERVES A TARGET: a program whose pinned build carries the
/// target's row serves it; one whose build has no such row, or that the channel pins nowhere,
/// does not (its hint said); a member of a coherence group does not where its partner does
/// not, since the tuple installs whole; a vendor row is its vendor's; and a manifest the store
/// never read is no verdict. NEGATIVE CONTROLS (2026-11-11): with `judge`'s partner search
/// removed, trust-ir read Served on Linux; with `judge_alone`'s `artifact_for` check replaced
/// by `Some(_) => Serving::Served`, trust read Served on Linux.
#[test]
fn the_signed_manifests_decide_what_serves_a_target() {
    let layout = learned_store("verdicts");
    let index = index();
    let linux = Targets::read_with(&layout, &index, LINUX, &source);
    assert_eq!(linux.of("ay"), &Serving::Served);
    assert_eq!(
        linux.unserved("trust"),
        Some(crate::state::unavailable(LINUX, "Trust is published for ARM macOS only").as_str())
    );
    assert_eq!(
        linux.unserved("trust-ir"),
        Some(
            crate::state::unavailable(
                LINUX,
                "it installs only together with trust, which has no build for it"
            )
            .as_str()
        ),
        "trust-ir's own Linux row does not install the tuple without trust's"
    );
    assert_eq!(linux.of("ty"), &Serving::Unknown, "never read: no verdict");
    assert_eq!(
        linux.unserved("unpinned"),
        Some(crate::state::unavailable(LINUX, "").as_str())
    );
    assert_eq!(
        linux.of("claude") == &Serving::Served,
        crate::vendor_direct::spec("claude").is_some_and(|spec| spec.serves(LINUX))
            || crate::alab::index_pin(&index, "claude", LINUX).is_some(),
        "a vendor row is its vendor's, or the ALab build the index pins for the target"
    );
    assert_eq!(linux.of("not-listed"), &Serving::Unknown);

    let mac = Targets::read_with(&layout, &index, MAC, &source);
    assert_eq!(mac.of("ay"), &Serving::Served);
    assert!(
        mac.unserved("trust-ir").is_some(),
        "no macOS row of its own"
    );
    assert!(
        mac.unserved("trust").is_some(),
        "trust's macOS pack does not install without trust-ir's"
    );
}

/// A CACHED PAIR IS RE-VERIFIED BEFORE IT IS BELIEVED: a manifest a key off the roster signed,
/// or one that names another program or build than the key it sits under, is no verdict, never
/// "unserved". NEGATIVE CONTROL (2026-11-11): with `verified` returning the parse without its
/// signature check (`parse_pkg(&testkit::machine_signed(raw))`), the outsider's ay read
/// Unserved.
#[test]
fn a_cached_pair_is_believed_only_once_it_verifies_as_what_it_names() {
    let layout = crate::testdir::layout("atpkg-serving-forged-");
    keep(
        &layout,
        "ay",
        5,
        &manifest("ay", 5, &[MAC]),
        &testkit::OUTSIDER_SEED,
    );
    keep(
        &layout,
        "ty",
        9,
        &manifest("ty", 8, &[MAC]),
        &testkit::MACHINE_SEED,
    );
    let linux = Targets::read_with(&layout, &index(), LINUX, &source);
    assert_eq!(linux.of("ay"), &Serving::Unknown, "signed off the roster");
    assert_eq!(
        linux.of("ty"),
        &Serving::Unknown,
        "build 8 under build 9's key"
    );
}

/// THE DEFAULT SET HERE LEAVES OUT WHAT NO BUILD SERVES HERE: `pkg doctor`'s toolset row (and
/// `aterm status`, which reads it) judges the listed set less every program the signed
/// manifests say has no build for the target, and keeps one with no verdict; the passes still
/// complete the listed set, whose prescan reads each manifest itself. NEGATIVE CONTROL
/// (2026-11-11): with `default_set_here`'s `retain` removed, trust, trust-ir and unpinned were
/// in the set here.
#[test]
fn the_default_set_here_leaves_out_what_no_build_serves_here() {
    let layout = learned_store("default-set");
    let index = index();
    let linux = Targets::read_with(&layout, &index, LINUX, &source);
    let listed = crate::cli::default_set_listed(&index, LINUX);
    let here = crate::cli::default_set_here(&index, &linux);
    assert_eq!(
        listed.difference(&here).cloned().collect::<Vec<_>>(),
        ["trust", "trust-ir", "unpinned"]
    );
    assert!(here.contains("ay") && here.contains("ty"), "{here:?}");
    let cfg = crate::config::PackagesConfig {
        exclude: Some(vec!["ay".into()]),
        ..Default::default()
    };
    let wanted = crate::cli::wanted_programs(&index, &cfg, &linux);
    assert_eq!(
        here.difference(&wanted).cloned().collect::<Vec<_>>(),
        ["ay"],
        "[packages].exclude narrows the set here and nothing else"
    );
    assert_eq!(
        crate::cli::default_set_here(&index, &Targets::unread(LINUX)),
        listed,
        "with no verdict nothing is set aside"
    );
}

/// A LOCK'S BUILD IS UNPUBLISHED HERE ONLY BY ITS SIGNED PACK: no pack for the target, or a pack
/// of another tree, is why no install can satisfy the lock; the pack the lock names, or a build
/// the store never read, is not. NEGATIVE CONTROL (2026-11-11): with the `tree_root` arm
/// removed, macOS's pack of another tree read as satisfying the lock.
#[test]
fn a_locks_build_is_unpublished_here_only_by_its_signed_pack() {
    let layout = learned_store("lock");
    let index = index();
    assert_eq!(
        lock_unpublished_with(&layout, &index, 16387, TREE, LINUX, &source),
        Some(crate::state::unavailable(
            LINUX,
            "Trust is published for ARM macOS only"
        ))
    );
    assert_eq!(
        lock_unpublished_with(&layout, &index, 16387, TREE, MAC, &source),
        None
    );
    assert_eq!(
        lock_unpublished_with(&layout, &index, 16387, &"b".repeat(64), MAC, &source),
        Some(format!(
            "its {MAC} pack is tree {}, not the tree the lock names",
            &TREE[..12]
        ))
    );
    assert_eq!(
        lock_unpublished_with(&layout, &index, 16388, TREE, LINUX, &source),
        None,
        "a build the store never read is no verdict"
    );
}

#[derive(Default)]
struct Published {
    manifests: RefCell<BTreeMap<String, Vec<u8>>>,
    reads: RefCell<Vec<String>>,
    offline: Cell<bool>,
}

impl crate::flow::Fetcher for Published {
    fn index_candidates(&self) -> Result<Vec<crate::select::Candidate>, String> {
        Err("no index here".into())
    }

    fn pkg_manifest(
        &self,
        _repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.reads.borrow_mut().push(program.to_string());
        if self.offline.get() {
            return Err("offline".into());
        }
        let raw = self
            .manifests
            .borrow()
            .get(&format!("{program}-{build}"))
            .cloned()
            .ok_or("not published")?;
        let signature = testkit::sign(&testkit::MACHINE_SEED, &raw);
        Ok((raw, signature))
    }

    fn download(&self, _: &str, _: &str, _: &std::path::Path) -> Result<(), String> {
        Err("no downloads here".into())
    }

    fn package_source_id(&self, _repo: &str, _program: &str) -> Option<String> {
        Some(SOURCE.to_string())
    }
}

/// THE UPDATE PASS LEARNS WHAT SERVES HERE, AND LEARNS IT AGAIN WHERE NOTHING DID: it reads and
/// keeps each pinned manifest it is asked about, skips a vendor row and one already serving here,
/// and reads again one whose kept copy lacks the target's row, so a row a builder merges onto
/// the published build is believed on the next pass; an offline read keeps what was known.
/// NEGATIVE CONTROL (2026-11-11): with `learn`'s `!= Serving::Served` filter removed, the second
/// pass read ay again; with it reading `judge` instead of `judge_alone`, the second pass read
/// trust-ir again although its own Linux row was kept.
#[test]
fn the_update_pass_learns_what_serves_here_and_again_where_nothing_did() {
    let layout = crate::testdir::layout("atpkg-serving-learn-");
    let index = index();
    let published = Published::default();
    for (program, build, targets) in [
        ("ay", 5, &[LINUX][..]),
        ("trust", 16387, &[MAC][..]),
        ("trust-ir", 7, &[LINUX][..]),
    ] {
        published.manifests.borrow_mut().insert(
            format!("{program}-{build}"),
            manifest(program, build, targets),
        );
    }
    let asked: BTreeSet<String> = ["ay", "trust", "trust-ir", "ty", "claude"]
        .map(String::from)
        .into();
    assert_eq!(
        Targets::read_with(&layout, &index, LINUX, &source).of("ay"),
        &Serving::Unknown
    );
    learn(&published, &layout, &index, LINUX, &asked);
    assert_eq!(
        *published.reads.borrow(),
        ["ay", "trust", "trust-ir", "ty"],
        "every index program asked about, never the vendor row"
    );
    let linux = Targets::read_with(&layout, &index, LINUX, &source);
    assert_eq!(linux.of("ay"), &Serving::Served);
    assert!(linux.unserved("trust").is_some() && linux.unserved("trust-ir").is_some());
    assert_eq!(
        linux.of("ty"),
        &Serving::Unknown,
        "not published: no verdict"
    );

    published.offline.set(true);
    learn(&published, &layout, &index, LINUX, &asked);
    let offline = Targets::read_with(&layout, &index, LINUX, &source);
    assert_eq!(offline, linux, "an offline pass keeps what was known");

    published.offline.set(false);
    published.reads.borrow_mut().clear();
    published.manifests.borrow_mut().insert(
        "trust-16387".into(),
        manifest("trust", 16387, &[MAC, LINUX]),
    );
    learn(&published, &layout, &index, LINUX, &asked);
    assert_eq!(*published.reads.borrow(), ["trust", "ty"]);
    let linux = Targets::read_with(&layout, &index, LINUX, &source);
    assert_eq!(
        linux.of("trust"),
        &Serving::Served,
        "the merged row is believed"
    );
    assert_eq!(linux.of("trust-ir"), &Serving::Served);
}
