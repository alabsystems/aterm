// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `rustc` group publish over fakes of its parts (the packers, the indexer, the seal's
//! `-Vv`, the receipt switch, the candidate gate) and real git checkouts in scratch: the
//! preflight collects every refusal before anything runs (the seal, the exposes, a live
//! tree, the store, the trust build and every sibling judged against the floor per target,
//! the force rules), and step 2 re-checks each sibling before any packs and before its
//! own.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use atpkg_publish::api::{
    BundlePacker, BundleRequest, IndexRequest, Indexer, Lineage, PackOutcome, PackRequest, Packer,
    Published, Signing,
};
use atpkg_publish::artifact::lineage_switch::Switch;
use atpkg_publish::channel::Log;
use atpkg_publish::channel::candidate::{CandidateGate, GateAnswer};
use atpkg_publish::channel::group::{
    GroupOutcome, GroupRequest, Mode, Parts, SealProbe, publish_group,
};
use atpkg_publish::ctx::{Ctx, STAGING_OWNER};
use atpkg_publish::error::Result;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::host::{HostError, Probe as Answer, Release, ReleaseHost, Replace, Slug};
use atpkg_publish::programs::TRUST_EXPOSES;
use atpkg_publish::triple::Triple;

use super::{Doc, Rig, git, repo};

const MAC: &str = "aarch64-apple-darwin";
const LINUX: &str = "x86_64-unknown-linux-gnu";
const SIBLINGS: [&str; 3] = ["trust-cg", "trust-ir", "trust-vc"];

/// The fake, except that one tag stops being visible once `hidden` is set.
struct Hiding {
    inner: Arc<FakeHost>,
    tag: &'static str,
    hidden: Arc<AtomicBool>,
}

impl ReleaseHost for Hiding {
    fn releases(&self, slug: &Slug) -> std::result::Result<Vec<Release>, HostError> {
        self.inner.releases(slug)
    }
    fn release(&self, slug: &Slug, tag: &str) -> std::result::Result<Release, HostError> {
        if tag == self.tag && self.hidden.load(Ordering::SeqCst) {
            return Err(HostError::NotFound);
        }
        self.inner.release(slug, tag)
    }
    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> std::result::Result<(), HostError> {
        self.inner.download(slug, tag, assets, dir)
    }
    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> std::result::Result<(), HostError> {
        self.inner.create(slug, tag, title, notes, files)
    }
    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> std::result::Result<(), HostError> {
        self.inner.upload(slug, tag, files, replace)
    }
    fn publish_draft(&self, slug: &Slug, tag: &str) -> std::result::Result<(), HostError> {
        self.inner.publish_draft(slug, tag)
    }
    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Answer {
        self.inner.probe_public(slug, tag, asset)
    }
    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Answer {
        self.inner.fetch_public(slug, tag, asset, dest, cap)
    }
    fn clone_repo(&self, slug: &Slug, dest: &Path) -> std::result::Result<(), HostError> {
        self.inner.clone_repo(slug, dest)
    }
    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> std::result::Result<(), HostError> {
        self.inner.fetch_repo(slug, dir)
    }
}

struct Probe {
    vv: RefCell<String>,
    switch: Switch,
}

impl SealProbe for Probe {
    fn trustc_vv(&self, _: &Path) -> std::result::Result<String, String> {
        Ok(self.vv.borrow().clone())
    }
    fn lineage_switch(&self, _: &Ctx, _: &Path) -> Switch {
        self.switch.clone()
    }
}

struct Gate {
    answer: GateAnswer,
    asked: RefCell<Vec<Option<u64>>>,
}

impl CandidateGate for Gate {
    fn admits(&self, _: &Ctx, _: &Path, pin: Option<u64>) -> GateAnswer {
        self.asked.borrow_mut().push(pin);
        self.answer.clone()
    }
}

/// A step a peer session takes while a pack runs.
type During = RefCell<Option<Box<dyn Fn()>>>;
/// The same, told which sibling is packing.
type DuringPack = RefCell<Option<Box<dyn Fn(&str)>>>;

/// Records every pack and index; `during_bundle` runs while the bundle "packs" (a peer
/// session moving a checkout), `during_pack` before each sibling pack returns.
#[derive(Default)]
struct Packs {
    bundles: RefCell<Vec<BundleRequest>>,
    packs: RefCell<Vec<PackRequest>>,
    indexes: RefCell<Vec<IndexRequest>>,
    /// The trust repo each pack was handed (`Ctx::trust_repo`, where it reads the switch).
    trust_repos: RefCell<Vec<PathBuf>>,
    during_bundle: During,
    during_pack: DuringPack,
}

fn outcome(program: &str, build: Option<u64>, triple: Option<Triple>) -> PackOutcome {
    PackOutcome {
        program: program.into(),
        build: build.unwrap_or(0),
        version: "1.0".into(),
        triple: triple.unwrap_or(atpkg_publish::triple::AARCH64_APPLE_DARWIN),
        asset: PathBuf::from("a.tar.zst"),
        manifest: PathBuf::from("pkg.toml"),
        signature: None,
        exposes: Vec::new(),
        lineage: Lineage::Unproven,
        uploaded: false,
    }
}

impl BundlePacker for Packs {
    fn pack_bundle(&self, ctx: &Ctx, r: &BundleRequest) -> Result<PackOutcome> {
        if let Some(f) = &*self.during_bundle.borrow() {
            f();
        }
        self.trust_repos.borrow_mut().push(ctx.trust_repo.clone());
        self.bundles.borrow_mut().push(r.clone());
        Ok(outcome(&r.program, r.build, r.triple))
    }
}

impl Packer for Packs {
    fn can_pack(&self) -> std::result::Result<(), String> {
        Ok(())
    }

    fn pack(&self, ctx: &Ctx, r: &PackRequest) -> Result<PackOutcome> {
        self.trust_repos.borrow_mut().push(ctx.trust_repo.clone());
        self.packs.borrow_mut().push(r.clone());
        if let Some(f) = &*self.during_pack.borrow() {
            f(&r.program);
        }
        Ok(outcome(&r.program, r.build, Some(r.triple)))
    }
}

impl Indexer for Packs {
    fn publish(&self, _: &Ctx, r: &IndexRequest) -> Result<Published> {
        self.indexes.borrow_mut().push(r.clone());
        Ok(Published {
            index_build: 35,
            dir: r.out.clone(),
            uploaded: r.upload,
        })
    }
}

struct Cut {
    rig: Rig,
    seal: PathBuf,
    trust_repo: PathBuf,
    commit: String,
    checkouts: BTreeMap<String, PathBuf>,
    probe: Probe,
    gate: Gate,
    packs: Packs,
}

fn vv(commit: &str, host: &str) -> String {
    format!(
        "trustc 1.97.0-trust (abc 2026-10-01)\nbinary: trustc\ncommit-hash: {commit}\n\
         commit-date: 2026-10-01\nhost: {host}\nrelease: 1.97.0-trust\n"
    )
}

/// A seal of trust build 5 (five commits), siblings at build 5, each sibling's staging
/// repo listing an old pack, and the channel head atpkg-index-34 pinning `pins`
/// (`overlay` for Linux).
fn cut(label: &str, pins: &[(&str, u64)], overlay: &[(&str, u64)]) -> Cut {
    let c = cut_unstaged(label, pins, overlay);
    for s in SIBLINGS {
        c.rig.host.put_asset(
            &staging(s),
            &format!("atpkg-{s}-1"),
            &format!("pkg-{s}-1.toml"),
            b"x".to_vec(),
        );
    }
    c
}

/// A sibling's repo on the staging owner.
fn staging(program: &str) -> Slug {
    let repo = atpkg_publish::programs::get(program).map_or(program, |p| p.repo);
    Slug::new(STAGING_OWNER, repo)
}

/// [`cut`], with no staging repo that lists.
fn cut_unstaged(label: &str, pins: &[(&str, u64)], overlay: &[(&str, u64)]) -> Cut {
    let rig = Rig::new(label);
    let mut doc = Doc::new(34, pins);
    if !overlay.is_empty() {
        doc = doc.overlay(LINUX, overlay);
    }
    if !pins.is_empty() {
        rig.publish_head(&doc);
    }
    let seal = rig.path("toolchains/trust-abc");
    let bin = seal.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::create_dir_all(seal.join("lib")).expect("lib");
    for tool in ["trustc", "targo"].iter().chain(TRUST_EXPOSES) {
        let p = bin.join(tool);
        std::fs::write(&p, "#!\n").expect("tool");
        set_exec(&p);
    }
    let trust_repo = rig.path("trust");
    let commit = repo(&trust_repo, 5);
    let mut checkouts = BTreeMap::new();
    for s in SIBLINGS {
        let dir = rig.path(s);
        repo(&dir, 5);
        checkouts.insert(s.to_string(), dir);
    }
    Cut {
        probe: Probe {
            vv: RefCell::new(vv(&commit, MAC)),
            switch: Switch::Dormant,
        },
        gate: Gate {
            answer: GateAnswer::Admitted("PASS (fixture receipt)".into()),
            asked: RefCell::new(Vec::new()),
        },
        packs: Packs::default(),
        rig,
        seal,
        trust_repo,
        commit,
        checkouts,
    }
}

fn set_exec(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

fn request(c: &Cut, mode: Mode) -> GroupRequest {
    GroupRequest {
        seal: c.seal.clone(),
        trust_repo: c.trust_repo.clone(),
        checkouts: c.checkouts.clone(),
        mode,
        force_same_build: false,
        allow_build_tree_seal: false,
        out: Some(c.rig.path("out")),
    }
}

fn run(c: &Cut, req: &GroupRequest) -> (Result<GroupOutcome>, Log) {
    let parts = Parts {
        probe: &c.probe,
        gate: &c.gate,
        bundle: &c.packs,
        packer: &c.packs,
        indexer: &c.packs,
    };
    let mut log = Rig::log();
    let r = publish_group(&c.rig.ctx, req, &parts, &mut log);
    (r, log)
}

fn ok(c: &Cut, req: &GroupRequest) -> (GroupOutcome, Log) {
    let (r, log) = run(c, req);
    (r.unwrap_or_else(|e| panic!("{e}\n{}", log.text())), log)
}

fn refused(c: &Cut, req: &GroupRequest) -> String {
    match run(c, req) {
        (Ok(o), log) => panic!("ran, but must be refused: {o:?}\n{}", log.text()),
        (Err(e), _) => e.to_string(),
    }
}

/// The floor everything is newer than: trust 3, each sibling 4.
const FLOOR: [(&str, u64); 5] = [
    ("ay", 8256),
    ("trust-ir", 4),
    ("trust-cg", 4),
    ("trust-vc", 4),
    ("trust", 3),
];

#[test]
fn a_dry_run_judges_everything_and_runs_nothing() {
    let c = cut("group-dry", &FLOOR, &[]);
    let (o, log) = ok(&c, &request(&c, Mode::DryRun));
    assert!(o.builds.is_empty() && o.index_build.is_none());
    assert!(c.packs.bundles.borrow().is_empty() && c.packs.packs.borrow().is_empty());
    assert!(c.packs.indexes.borrow().is_empty());
    let text = log.text();
    assert!(text.contains("mode: dry-run"), "{text}");
    assert!(text.contains("→ trust build 5"), "{text}");
    assert!(
        text.contains("channel floor: atpkg-index-34 (pins trust 3)"),
        "{text}"
    );
    assert!(
        text.contains(
            "sibling trust-ir: this cut packs build 5 (the channel head atpkg-index-34 pins 4)"
        ),
        "{text}"
    );
    assert!(text.contains("dry run — nothing executed"), "{text}");
    assert!(text.contains("the seed pin: NOT ENFORCED"), "{text}");
    assert!(
        c.gate.asked.borrow().is_empty(),
        "asking the gate builds the cutter; a dry run builds nothing"
    );
    assert!(
        text.contains(&format!(
            "the candidate gate (D27): publishing (--upload) needs its PASS — not asked by a dry \
             run (`targo --unverified ship trust candidate --admits {} --base-build 3`)",
            c.seal.display()
        )),
        "{text}"
    );
    ok(&c, &request(&c, Mode::Stage));
    assert_eq!(
        *c.gate.asked.borrow(),
        [Some(3)],
        "a stage asks the gate, against the published pin"
    );
}

#[test]
fn a_staged_cut_packs_the_bundle_then_the_siblings_with_the_seals_toolchain_then_indexes() {
    let c = cut("group-stage", &FLOOR, &[]);
    let (o, _) = ok(&c, &request(&c, Mode::Stage));
    let bundles = c.packs.bundles.borrow();
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].program, "trust");
    assert_eq!(bundles[0].build, Some(5));
    assert_eq!(bundles[0].version.as_deref(), Some("1.97.0-trust"));
    assert_eq!(bundles[0].triple.map(Triple::as_str), Some(MAC));
    assert_eq!(bundles[0].signing, Signing::MachineKey);
    assert!(!bundles[0].upload);
    let packs = c.packs.packs.borrow();
    assert_eq!(
        packs.iter().map(|p| p.program.as_str()).collect::<Vec<_>>(),
        SIBLINGS
    );
    for p in packs.iter() {
        assert_eq!(p.build, Some(5));
        assert_eq!(p.toolchain.as_deref(), Some(c.seal.join("bin").as_path()));
    }
    // The packers read the lineage switch from the repo the preflight judged the seal
    // under (`--trust-repo`), never the ctx's default $HOME/trust.
    assert_ne!(
        c.trust_repo, c.rig.ctx.trust_repo,
        "the fixture's repo is not the default"
    );
    assert_eq!(
        *c.packs.trust_repos.borrow(),
        vec![c.trust_repo.clone(); 1 + SIBLINGS.len()]
    );
    let indexes = c.packs.indexes.borrow();
    assert_eq!(indexes.len(), 1);
    let d = &indexes[0].delta;
    assert_eq!(
        d.pins,
        BTreeMap::from([
            ("trust".to_string(), 5),
            ("trust-cg".to_string(), 5),
            ("trust-ir".to_string(), 5),
            ("trust-vc".to_string(), 5)
        ])
    );
    assert!(
        d.target_pins.is_empty(),
        "every other pin is carried from the head"
    );
    assert!(!indexes[0].upload);
    assert_eq!(o.index_build, Some(35));
    let ready = std::fs::read_to_string(c.rig.path("out/READY.md")).expect("READY.md");
    assert!(
        ready.contains("atpkg-publish publish-group rustc --seal"),
        "{ready}"
    );
    assert!(ready.contains("SIGNED by this machine's key"), "{ready}");
}

#[test]
fn off_the_pin_triple_the_four_builds_ride_that_targets_overlay() {
    let c = cut("group-linux", &FLOOR, &[("trust-ir", 1)]);
    *c.probe.vv.borrow_mut() = vv(&c.commit, LINUX);
    let (_, log) = ok(&c, &request(&c, Mode::Stage));
    let text = log.text();
    assert!(
        text.contains(&format!("sibling trust-ir: this cut packs build 5 (the channel head atpkg-index-34 pins 1 for {LINUX})")),
        "{text}"
    );
    assert!(
        text.contains(&format!("sibling trust-cg: this cut packs build 5 (the channel head atpkg-index-34 pins no trust-cg for {LINUX} — the index step adds it as new)")),
        "{text}"
    );
    let indexes = c.packs.indexes.borrow();
    let d = &indexes[0].delta;
    assert!(d.pins.is_empty(), "`pin` belongs to the Mac");
    let linux = Triple::parse(LINUX).expect("triple");
    assert_eq!(d.target_pins[&linux].len(), 4);
}

#[test]
fn a_sibling_behind_or_level_with_the_floor_is_refused_before_anything_packs() {
    let behind = [
        ("trust-ir", 7),
        ("trust-cg", 1),
        ("trust-vc", 1),
        ("trust", 3),
    ];
    let c = cut("group-behind", &behind, &[]);
    let e = refused(&c, &request(&c, Mode::Stage));
    assert!(
        e.contains("trust-ir: this cut would pack build 5, BELOW the 7 that the channel head atpkg-index-34 pins"),
        "{e}"
    );
    assert_eq!(
        e.matches("this cut would pack").count(),
        1,
        "only trust-ir is behind: {e}"
    );
    assert!(
        c.packs.bundles.borrow().is_empty(),
        "nothing packs after a refusal"
    );
    let mut forced = request(&c, Mode::Stage);
    forced.force_same_build = true;
    assert!(
        refused(&c, &forced).contains("BELOW the 7"),
        "force never admits a lower build"
    );

    // Level with the floor (and the compiler with it): refused unless forced.
    let level = [
        ("trust-ir", 5),
        ("trust-cg", 5),
        ("trust-vc", 5),
        ("trust", 5),
    ];
    let c = cut("group-level", &level, &[]);
    let e = refused(&c, &request(&c, Mode::Stage));
    assert!(
        e.contains("the seal's trust build 5 is the SAME build"),
        "{e}"
    );
    assert!(
        e.contains("trust-ir: this cut would pack build 5, the SAME build"),
        "{e}"
    );
    let mut forced = request(&c, Mode::Stage);
    forced.force_same_build = true;
    let (_, log) = ok(&c, &forced);
    assert!(
        log.contains("--force-same-build: trust-ir repacks build 5"),
        "{}",
        log.text()
    );
}

#[test]
fn the_seals_trust_build_is_held_to_the_floor() {
    let c = cut("group-trust-behind", &[("trust", 9), ("trust-ir", 1)], &[]);
    let e = refused(&c, &request(&c, Mode::Stage));
    assert!(e.contains("the seal's trust build 5 is BELOW the 9"), "{e}");
    let mut forced = request(&c, Mode::Stage);
    forced.force_same_build = true;
    assert!(refused(&c, &forced).contains("BELOW the 9"));
}

/// When the compiler moved, each sibling takes the first build not yet published — the
/// number judged is the number packed; and an equal build then means the tag probe
/// answered wrong, which no flag lifts.
#[test]
fn a_moved_compiler_skips_published_numbers_and_never_reuses_one() {
    let floor = [
        ("trust-ir", 4),
        ("trust-cg", 5),
        ("trust-vc", 4),
        ("trust", 3),
    ];
    let c = cut("group-moved", &floor, &[]);
    c.rig.host.put_asset(
        &Slug::new("alabsystems", "trust-cg"),
        "atpkg-trust-cg-5",
        "pkg-trust-cg-5.toml",
        b"x".to_vec(),
    );
    // A number taken only on the staging owner is taken too (5 and 6 there).
    for n in [5, 6] {
        c.rig.host.put_asset(
            &staging("trust-vc"),
            &format!("atpkg-trust-vc-{n}"),
            &format!("pkg-trust-vc-{n}.toml"),
            b"x".to_vec(),
        );
    }
    let (_, log) = ok(&c, &request(&c, Mode::Stage));
    assert!(
        log.contains(
            "sibling trust-cg: this cut packs build 6 (the channel head atpkg-index-34 pins 5)"
        ),
        "{}",
        log.text()
    );
    let packed = |program: &str| {
        c.packs
            .packs
            .borrow()
            .iter()
            .find(|p| p.program == program)
            .and_then(|p| p.build)
    };
    assert_eq!(packed("trust-cg"), Some(6));
    assert_eq!(packed("trust-vc"), Some(7));
    assert_eq!(packed("trust-ir"), Some(5));

    // The probe misses the published tag: an equal number from a new compiler.
    let c = cut("group-moved-blind", &floor, &[]);
    let mut forced = request(&c, Mode::Stage);
    forced.force_same_build = true;
    let e = refused(&c, &forced);
    assert!(
        e.contains("trust-cg: this cut would pack build 5, the SAME build that the channel head atpkg-index-34 pins, from a NEW compiler (trust 5 over the published 3)"),
        "{e}"
    );
}

#[test]
fn the_seal_preflights_refuse_by_name() {
    let c = cut("group-seal", &FLOOR, &[]);
    std::fs::remove_file(c.seal.join("bin/trust-wp-rustc")).expect("rm");
    let e = refused(&c, &request(&c, Mode::DryRun));
    assert!(
        e.contains("missing 1 exposed tool(s) — trust-wp-rustc"),
        "{e}"
    );

    let c = cut("group-nohost", &FLOOR, &[]);
    *c.probe.vv.borrow_mut() = format!("commit-hash: {}\nrelease: x\n", c.commit);
    let e = refused(&c, &request(&c, Mode::DryRun));
    assert!(e.contains("reports no host line"), "{e}");
    assert!(!e.contains("sibling"), "{e}");

    let c = cut("group-noexec", &FLOOR, &[]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            c.seal.join("bin/targo"),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("chmod");
        assert!(refused(&c, &request(&c, Mode::DryRun)).contains("no executable bin/targo"));
    }

    // A live build tree, and the store.
    let c = cut("group-live", &FLOOR, &[]);
    let live = c.rig.path("trust/build/aarch64-apple-darwin/stage2");
    std::fs::create_dir_all(live.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&c.seal, &live).expect("move");
    let mut req = request(&c, Mode::DryRun);
    req.seal = live.clone();
    assert!(refused(&c, &req).contains("is a live build tree"));
    req.allow_build_tree_seal = true;
    let (_, log) = ok(&c, &req);
    assert!(log.contains("--allow-build-tree-seal: packing from the live build tree"));
    // `*/build/*/stage2` matches however deep the stage2 sits below `build`.
    let deeper = c
        .rig
        .path("trust/build/aarch64-apple-darwin/stage-out/stage2");
    std::fs::create_dir_all(deeper.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&live, &deeper).expect("move");
    let mut req = request(&c, Mode::DryRun);
    req.seal = deeper;
    assert!(refused(&c, &req).contains("is a live build tree"));

    let c = cut("group-store", &FLOOR, &[]);
    let store = c.rig.ctx.pkg_prefix.join("store/trust/9192");
    std::fs::create_dir_all(store.parent().expect("parent")).expect("mkdir");
    std::fs::rename(&c.seal, &store).expect("move");
    let mut req = request(&c, Mode::DryRun);
    req.seal = store;
    assert!(refused(&c, &req).contains("resolves INSIDE the atpkg store"));
}

#[test]
fn the_trust_repo_must_hold_the_seals_commit() {
    let c = cut("group-repo", &FLOOR, &[]);
    let mut req = request(&c, Mode::DryRun);
    req.trust_repo = c.rig.path("nowhere");
    assert!(refused(&c, &req).contains("the trust repo is not a checkout"));
    *c.probe.vv.borrow_mut() = vv("9999999999999999999999999999999999999999", MAC);
    let e = refused(&c, &request(&c, Mode::DryRun));
    assert!(e.contains("is not in"), "{e}");
}

/// No floor: a dry run says so; staging or publishing is refused before anything packs —
/// without the head no build is judged and no published number is skipped.
#[test]
fn no_floor_is_said_by_a_dry_run_and_refused_by_a_cut() {
    let c = cut("group-nofloor", &[], &[]);
    let (_, log) = ok(&c, &request(&c, Mode::DryRun));
    assert!(
        log.contains("no floor index to judge the builds against"),
        "{}",
        log.text()
    );
    assert!(!log.contains("sibling trust-ir:"));
    for mode in [Mode::Stage, Mode::Upload] {
        let e = refused(&c, &request(&c, mode));
        assert!(
            e.contains("no floor index to judge the builds against")
                && e.contains("the packs would run for nothing"),
            "{mode:?}: {e}"
        );
    }
    assert!(c.packs.bundles.borrow().is_empty());
}

/// A head past its horizon still pins what it pins: it is the floor (and re-signing it is
/// the index step's job).
#[test]
fn a_lapsed_head_is_still_the_floor() {
    let c = cut("group-lapsed", &[], &[]);
    let mut doc = Doc::new(34, &FLOOR);
    doc.valid_until = "2026-01-01T00:00:00Z".into();
    c.rig.publish_head(&doc);
    let (_, log) = ok(&c, &request(&c, Mode::Stage));
    assert!(
        log.contains("channel floor: atpkg-index-34 (pins trust 3)"),
        "{}",
        log.text()
    );
    assert!(log.contains(
        "sibling trust-ir: this cut packs build 5 (the channel head atpkg-index-34 pins 4)"
    ));
}

/// The staging owner's repos are private: one that does not list for this identity is a
/// refusal, never "no build number taken there".
#[test]
fn a_staging_repo_this_identity_cannot_see_is_refused_when_the_compiler_moved() {
    let c = cut_unstaged("group-unstaged", &FLOOR, &[]);
    let e = refused(&c, &request(&c, Mode::DryRun));
    for s in SIBLINGS {
        assert!(
            e.contains(&format!("{} does not list for this identity", staging(s))),
            "{e}"
        );
    }
    assert!(e.contains("~/.secrets/gh_access_token"), "{e}");
    // Not asked when the compiler did not move: the build is the revision count.
    let level = [
        ("trust-ir", 4),
        ("trust-cg", 4),
        ("trust-vc", 4),
        ("trust", 5),
    ];
    let c = cut_unstaged("group-unstaged-level", &level, &[]);
    let mut forced = request(&c, Mode::DryRun);
    forced.force_same_build = true;
    ok(&c, &forced);
}

/// STEP 2 RE-CHECKS THE JUDGMENT, before ANY sibling packs and again before each.
#[test]
fn a_sibling_that_moves_while_the_bundle_packs_stops_the_cut() {
    let c = cut("group-moved-checkout", &FLOOR, &[]);
    let dir = c.checkouts["trust-ir"].clone();
    *c.packs.during_bundle.borrow_mut() = Some(Box::new(move || {
        std::fs::write(dir.join("g"), "x").expect("write");
        git(&dir, &["add", "g"]);
        git(&dir, &["commit", "-q", "-m", "a peer's commit"]);
    }));
    let e = refused(&c, &request(&c, Mode::Stage));
    assert!(
        e.contains("trust-ir:") && e.contains("moved since the preflight"),
        "{e}"
    );
    assert!(
        c.packs.packs.borrow().is_empty(),
        "no sibling packs from a moved checkout"
    );
    assert!(c.packs.indexes.borrow().is_empty());

    let c = cut("group-dirty", &FLOOR, &[]);
    let dir = c.checkouts["trust-vc"].clone();
    *c.packs.during_bundle.borrow_mut() = Some(Box::new(move || {
        std::fs::write(dir.join("f"), "dirty").expect("write");
    }));
    let e = refused(&c, &request(&c, Mode::Stage));
    assert!(
        e.contains("trust-vc:") && e.contains("became dirty since the preflight"),
        "{e}"
    );
    assert!(
        c.packs.packs.borrow().is_empty(),
        "every sibling is re-checked before ANY packs"
    );

    // A lower number than judged: the probe stops seeing the published tag.
    let floor = [
        ("trust-ir", 4),
        ("trust-cg", 5),
        ("trust-vc", 4),
        ("trust", 3),
    ];
    let mut c = cut("group-lower", &floor, &[]);
    let public = Slug::new("alabsystems", "trust-cg");
    c.rig.host.put_asset(
        &public,
        "atpkg-trust-cg-5",
        "pkg-trust-cg-5.toml",
        b"x".to_vec(),
    );
    let hidden = Arc::new(AtomicBool::new(false));
    let inner = c.rig.host.clone();
    c.rig.route(Arc::new(Hiding {
        inner,
        tag: "atpkg-trust-cg-5",
        hidden: hidden.clone(),
    }));
    *c.packs.during_bundle.borrow_mut() = Some(Box::new(move || {
        hidden.store(true, Ordering::SeqCst);
    }));
    let e = refused(&c, &request(&c, Mode::Stage));
    assert!(
        e.contains("trust-cg: step 2 would now pack build 5, BELOW the 6 the preflight judged"),
        "{e}"
    );
    assert!(c.packs.packs.borrow().is_empty());

    // Between two sibling packs: the later one is caught, the earlier one already packed.
    let c = cut("group-between", &FLOOR, &[]);
    let later = c.checkouts["trust-vc"].clone();
    *c.packs.during_pack.borrow_mut() = Some(Box::new(move |program: &str| {
        if program == "trust-cg" {
            std::fs::write(later.join("h"), "x").expect("write");
            git(&later, &["add", "h"]);
            git(&later, &["commit", "-q", "-m", "moved"]);
        }
    }));
    let e = refused(&c, &request(&c, Mode::Stage));
    assert!(
        e.contains("trust-vc:") && e.contains("moved since the preflight"),
        "{e}"
    );
    let packed: Vec<String> = c
        .packs
        .packs
        .borrow()
        .iter()
        .map(|p| p.program.clone())
        .collect();
    assert_eq!(packed, ["trust-cg", "trust-ir"]);
}

#[test]
fn the_candidate_gate_decides_a_publish_and_informs_a_stage() {
    let mut c = cut("group-gate", &FLOOR, &[]);
    c.gate.answer = GateAnswer::Refused(vec![
        "no candidate-gate receipt names this seal".into(),
        "second reason".into(),
    ]);
    let (_, log) = ok(&c, &request(&c, Mode::Stage));
    assert!(log.contains(
        "the candidate gate (D27): publishing (--upload) is refused until it passes — no candidate-gate receipt names this seal"
    ));
    let e = refused(&c, &request(&c, Mode::Upload));
    assert!(
        e.contains("no candidate-gate receipt names this seal") && e.contains("second reason"),
        "{e}"
    );
    c.gate.answer = GateAnswer::CannotCheck("the gate could not check".into());
    assert!(refused(&c, &request(&c, Mode::Upload)).contains("the gate could not check"));
    c.gate.answer = GateAnswer::Admitted("PASS (r)".into());
    let (_, log) = ok(&c, &request(&c, Mode::Stage));
    assert!(log.contains("the candidate gate (D27): PASS for this seal — PASS (r)"));
}

#[test]
fn the_seed_receipt_is_required_unless_the_switch_reads_dormant() {
    let mut c = cut("group-seed", &FLOOR, &[]);
    c.probe.switch = Switch::Unknown("no answer".into());
    let e = refused(&c, &request(&c, Mode::DryRun));
    for line in atpkg::lineage::seed_mint_refusals(&c.seal, &c.commit) {
        assert!(
            e.contains(&line),
            "one problem per checker line ({line}): {e}"
        );
    }
    c.probe.switch = Switch::Live;
    let e = refused(&c, &request(&c, Mode::DryRun));
    let lines = atpkg::lineage::seed_mint_refusals(&c.seal, &c.commit);
    assert!(!lines.is_empty(), "a seal with no receipt is refused");
    for line in lines {
        assert!(e.contains(&line), "the live switch enforces ({line}): {e}");
    }
}

#[test]
fn upload_needs_the_key_and_the_channel_token_and_a_keyless_stage_signs_nothing() {
    let c = cut("group-identity", &FLOOR, &[]);
    let e = refused(&c, &request(&c, Mode::Upload));
    assert!(
        e.contains("--upload needs the staging owner's token at ~/.secrets/gh_access_token"),
        "{e}"
    );
    // The control: both tokens and the key, and it publishes.
    let control = cut("group-identity-ok", &FLOOR, &[]);
    std::fs::write(&control.rig.ctx.tokens.staging, "staging-token\n").expect("token");
    let (o, _) = ok(&control, &request(&control, Mode::Upload));
    assert_eq!(o.index_build, Some(35));
    assert!(control.packs.indexes.borrow()[0].upload);

    std::fs::remove_file(&c.rig.ctx.tokens.channel).expect("rm token");
    assert!(refused(&c, &request(&c, Mode::Upload)).contains("needs the public owner's token"));
    std::fs::remove_file(&c.rig.ctx.identity.key).expect("rm key");
    assert!(refused(&c, &request(&c, Mode::Upload)).contains("needs a rostered machine key"));

    let (o, log) = ok(&c, &request(&c, Mode::Stage));
    assert!(o.index_build.is_none());
    assert!(c.packs.indexes.borrow().is_empty(), "no key, no index");
    assert!(
        c.packs
            .packs
            .borrow()
            .iter()
            .all(|p| p.signing == Signing::Unsigned)
    );
    assert!(log.contains("STAGED UNSIGNED"), "{}", log.text());
    let ready = std::fs::read_to_string(c.rig.path("out/READY.md")).expect("READY.md");
    assert!(ready.contains("UNSIGNED"), "{ready}");
}
