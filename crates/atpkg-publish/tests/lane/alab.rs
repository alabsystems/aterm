// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! End-to-end Apple lane tests over the fake channel and real git.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use atpkg_publish::api::Build;
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::{Error, Result};
use atpkg_publish::lane::LaneKind;
use atpkg_publish::lane::alab::{self, Decision, Deps, Options, Outcome, Row};
use atpkg_publish::lane::gap::PinCommit;
use atpkg_publish::lane::report::Transcript;
use atpkg_publish::lane::state::LaneState;
use atpkg_publish::triple::{AARCH64_APPLE_DARWIN as ARM, X86_64_APPLE_DARWIN as X86};

use super::super::shared::{Made, made_once, scratch};
use super::fixture::{self, Fx, Held, Release, git};

/// Test fixture world with index 5 pinning `ay`, `nn`, `trust-ir`, `trust`, `trust-mc`, and `claude`.
struct World {
    fx: Fx,
    pins: Mutex<BTreeMap<(String, Build), PinCommit>>,
    opts: Mutex<Options>,
}

fn sha_at(dir: &Path, build: u64) -> String {
    let all = git(dir, &["rev-list", "--reverse", "HEAD"]);
    all.lines()
        .nth(usize::try_from(build).expect("small") - 1)
        .expect("a commit at that build")
        .to_string()
}

fn at(history: &[String], build: u64) -> &str {
    &history[usize::try_from(build).expect("small") - 1]
}

impl World {
    fn new(label: &str) -> Self {
        Self::new_at(scratch(label))
    }

    fn new_at(tmp: aterm_tempfile::TempDir) -> Self {
        let fx = Fx::at(tmp);
        let (_, ay) = fx.dev_repo("ay", 4);
        fx.release("ay", "v0.1.0", at(&ay, 2), Release::GOOD);
        fx.release("ay", "v0.2.0", at(&ay, 3), Release::GOOD);
        fx.release(
            "ay",
            "v0.3.0",
            at(&ay, 4),
            Release {
                prerelease: true,
                ..Release::GOOD
            },
        );
        fx.release(
            "ay",
            "v0.4.0",
            at(&ay, 4),
            Release {
                signed: false,
                ..Release::GOOD
            },
        );
        let (_, nn) = fx.dev_repo("nn", 2);
        fx.release("nn", "v0.1.0", at(&nn, 2), Release::GOOD);
        let (_, mc) = fx.dev_repo("trust-mc", 4);
        fx.release("trust-mc", "v0.6.0", at(&mc, 4), Release::GOOD);
        let (_, trust) = fx.dev_repo("trust", 7);
        fx.release("trust", "v0.9.0", at(&trust, 7), Release::GOOD);
        let (_, ir) = fx.dev_repo("trust-ir", 2);
        fx.release("trust-ir", "v0.3.0", at(&ir, 2), Release::GOOD);
        fx.publish_head(
            5,
            &[
                ("ay", 2),
                ("nn", 2),
                ("trust-ir", 1),
                ("trust", 5),
                ("trust-mc", 4),
                ("claude", 2_026_090_101),
            ],
        );
        Self::of(fx)
    }

    fn of(fx: Fx) -> Self {
        let opts = Options::new(&fx.ctx, false);
        Self {
            fx,
            pins: Mutex::new(BTreeMap::new()),
            opts: Mutex::new(opts),
        }
    }

    /// [`World::new`] run until every program sits at its pin (cached once per process via [`Fx::laid`]).
    fn settled(label: &str) -> Self {
        static MADE: Made<(), Held> = Mutex::new(BTreeMap::new());
        let mut tmp = Some(scratch(label));
        let dir = tmp.as_ref().expect("a scratch dir").path().to_path_buf();
        let mut made = None;
        let held = made_once(&MADE, &(), &dir, |_| {
            let w = Self::new_at(tmp.take().expect("a scratch dir"));
            ok(w.run(false));
            w.fx.stage("trust", 7, &[ARM]);
            let (o, _) = ok(w.run(false));
            assert_eq!(o.published, Some(7));
            let held = w.fx.held();
            made = Some(w);
            held
        });
        made.unwrap_or_else(|| Self::of(Fx::laid(tmp.take().expect("a scratch dir"), &held)))
    }

    fn dev(&self, repo: &str) -> PathBuf {
        self.fx.path(&format!("dev/{repo}"))
    }

    fn run_as(&self, ctx: &Ctx, dry_run: bool) -> (Result<Outcome>, Transcript) {
        let tx = Transcript::memory(alab::ME);
        let pins = &self.pins;
        let pin_commit = move |_: &Ctx, p: &str, b: Build| {
            pins.lock().expect("pins").get(&(p.to_string(), b)).cloned()
        };
        let deps = Deps {
            packer: &self.fx.packer,
            indexer: &self.fx.indexer,
            notifier: &self.fx.notes,
            pin_commit: &pin_commit,
        };
        let mut opts = self.opts.lock().expect("opts").clone();
        opts.dry_run = dry_run;
        (alab::run(ctx, &opts, &deps, &tx), tx)
    }

    fn run(&self, dry_run: bool) -> (Result<Outcome>, Transcript) {
        self.run_as(&self.fx.ctx, dry_run)
    }

    fn state(&self) -> LaneState {
        LaneState::of(LaneKind::Alab, &self.fx.ctx.state_dir)
    }
}

fn row<'a>(o: &'a Outcome, p: &str) -> &'a Row {
    o.rows
        .iter()
        .find(|r| r.program.name == p)
        .unwrap_or_else(|| panic!("no {p} row"))
}

fn rendered(o: &Outcome, p: &str) -> String {
    let r = row(o, p);
    let gap = o.gaps.iter().find(|g| g.program == p);
    r.render(o.head.unwrap_or(0), "alabsystems", gap)
}

fn ok(r: (Result<Outcome>, Transcript)) -> (Outcome, Transcript) {
    match r.0 {
        Ok(o) => (o, r.1),
        Err(e) => panic!("the run refused: {e}\n{}", r.1.text()),
    }
}

#[test]
fn a_dry_run_before_any_clone_decides_nothing_and_writes_nothing() {
    let w = World::new("alab-dry");
    let (o, _) = ok(w.run(true));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why) if why.contains("no scratch clone at")),
        "{:?}",
        row(&o, "ay").decision
    );
    assert!(w.fx.calls("clone ").is_empty(), "a dry run clones nothing");
    let state = w.state();
    assert!(!state.dir.join("progs").exists());
    assert!(!state.index_seen.exists(), "a dry run writes no state");
    assert!(w.fx.notes.shown().is_empty(), "a dry run notifies nobody");
}

#[test]
fn an_unreadable_channel_is_could_not_run_only_for_a_dry_run() {
    let w = World::new("alab-dry-unreadable");
    let public = w.fx.ctx.accounts.public.clone();
    w.fx.host.set_unreachable(&public, true);
    let (dry, tx) = w.run(true);
    let err = dry.expect_err("a dry run over an unreadable channel");
    assert!(matches!(err, Error::CannotRun(_)), "{err}");
    assert_eq!(err.exit_code(), 3, "{err}\n{}", tx.text());
    assert!(!w.state().index_seen.exists(), "a dry run writes no state");
    let (o, tx) = ok(w.run(false));
    assert!(o.head.is_none(), "nothing decided: {}", tx.text());
    assert!(
        tx.text().contains("consecutive failures: 1") && tx.text().contains("exiting 0"),
        "{}",
        tx.text()
    );
}

#[test]
fn the_first_run_packs_both_apple_targets_and_holds_the_rustc_group() {
    let w = World::new("alab-first");
    let (o, _) = ok(w.run(false));
    let ay = rendered(&o, "ay");
    assert!(ay.starts_with("PACK: v0.2.0 (build 3) > pin 2"), "{ay}");
    assert!(
        !ay.contains("v0.3.0") && !ay.contains("v0.4.0"),
        "neither the prerelease nor the unsigned release is elected: {ay}"
    );
    assert_eq!(rendered(&o, "nn"), "OK: pin 2 is v0.1.0");
    assert_eq!(rendered(&o, "trust-mc"), "OK: pin 4 is v0.6.0");
    assert!(
        rendered(&o, "trust").starts_with("BUNDLE-LANE: v0.9.0 (build 7) > pin 5"),
        "{}",
        rendered(&o, "trust")
    );
    assert!(
        rendered(&o, "clean").starts_with("SKIP: not pinned by index-5"),
        "an unpinned program is a human decision"
    );
    let legs = w.fx.packer.legs();
    let out = w.state().dir.join("out/ay");
    assert_eq!(legs.len(), 3, "{legs:?}");
    assert_eq!(
        (legs[0].0.as_str(), legs[0].1, legs[0].2.clone()),
        ("ay", ARM, None)
    );
    assert_eq!(
        (legs[1].0.as_str(), legs[1].1, legs[1].2.clone()),
        ("ay", X86, Some(out.join("merge-base-ay-3.toml")))
    );
    assert_eq!((legs[2].0.as_str(), legs[2].1), ("trust-ir", ARM));
    let staged = String::from_utf8(
        w.fx.staging_asset("ay", 3, "pkg-ay-3.toml")
            .expect("staged"),
    )
    .expect("utf8");
    assert!(staged.contains("aarch64-apple-darwin") && staged.contains("x86_64-apple-darwin"));
    assert!(
        w.fx.staging_asset("trust-ir", 2, "pkg-trust-ir-2.toml")
            .is_some()
    );
    assert_eq!(o.held, ["trust-ir"]);
    assert_eq!(
        w.fx.indexer.deltas(),
        [BTreeMap::from([("ay".to_string(), 3)])]
    );
    assert_eq!(o.published, Some(6));
    let state = w.state();
    assert_eq!(state.index_seen().expect("seen"), 6);
    let pins_seen = std::fs::read_to_string(&state.pins_seen).expect("pins seen");
    assert!(pins_seen.lines().any(|l| l == "ay 3"), "{pins_seen}");
}

#[test]
fn a_held_build_on_staging_reads_index_and_is_never_repacked() {
    let w = World::new("alab-hold");
    ok(w.run(false));
    let packs = w.fx.packer.legs().len();
    let (o, _) = ok(w.run(false));
    assert_eq!(rendered(&o, "ay"), "OK: pin 3 is v0.2.0");
    assert!(
        rendered(&o, "trust-ir")
            .starts_with("INDEX: v0.3.0 (build 2) is already on alabsystems as atpkg-trust-ir-2"),
        "{}",
        rendered(&o, "trust-ir")
    );
    assert_eq!(w.fx.packer.legs().len(), packs, "nothing is re-packed");
    assert_eq!(
        w.fx.indexer.count(),
        1,
        "a held-only run publishes no index"
    );
}

#[test]
fn the_bundle_landing_on_staging_moves_the_whole_group_together() {
    let w = World::new("alab-group");
    ok(w.run(false));
    w.fx.stage("trust", 7, &[ARM]);
    let (o, _) = ok(w.run(false));
    assert!(rendered(&o, "trust").starts_with("INDEX: v0.9.0 (build 7)"));
    assert!(o.held.is_empty());
    assert_eq!(
        w.fx.indexer.deltas().last().expect("published"),
        &BTreeMap::from([("trust".to_string(), 7), ("trust-ir".to_string(), 2)])
    );
    assert_eq!(o.published, Some(7));
}

#[test]
fn a_pin_ahead_of_the_newest_signed_release_is_ok_and_a_clean_no_op() {
    let fx = Fx::new("alab-ahead");
    let (_, nn) = fx.dev_repo("nn", 1);
    fx.release("nn", "v0.1.1", at(&nn, 1), Release::GOOD);
    fx.publish_head(5, &[("nn", 2)]);
    let w = World {
        opts: Mutex::new(Options::new(&fx.ctx, false)),
        fx,
        pins: Mutex::new(BTreeMap::new()),
    };
    let (o, _) = ok(w.run(false));
    assert_eq!(
        rendered(&o, "nn"),
        "OK: pin 2 is AHEAD of the newest signed release v0.1.1 (build 1) — a pack cut past the \
         last promote"
    );
    assert_eq!(w.fx.indexer.count(), 0);
    assert!(
        w.fx.notes.shown().is_empty(),
        "an AHEAD pin notifies nobody"
    );
}

#[test]
fn a_pin_that_fell_on_the_channel_refuses_everything() {
    let w = World::settled("alab-pin-down");
    w.fx.publish_head(
        8,
        &[
            ("ay", 3),
            ("nn", 1),
            ("trust-ir", 2),
            ("trust", 7),
            ("trust-mc", 4),
        ],
    );
    let (r, _) = w.run(false);
    let e = r.expect_err("a pin downgrade refuses");
    assert_eq!(e.exit_code(), 1);
    assert!(
        e.to_string()
            .contains("PIN DOWNGRADE on the channel: nn 2 -> 1 in index-8"),
        "{e}"
    );
    assert_eq!(w.fx.indexer.count(), 2, "nothing more is signed");
}

#[test]
fn an_index_head_below_the_one_processed_refuses_everything() {
    let w = World::new("alab-index-down");
    ok(w.run(false));
    w.fx.host
        .set_draft(&w.fx.ctx.index_slug(), "atpkg-index-6", true);
    let (r, _) = w.run(false);
    let e = r.expect_err("an index downgrade refuses");
    assert!(
        e.to_string()
            .contains("channel head is index 5 but this lane already processed 6"),
        "{e}"
    );
    assert_eq!(w.fx.packer.legs().len(), 3, "nothing is packed");
}

fn ay_release(w: &World, tag: &str) -> u64 {
    let ay = w.dev("ay");
    fixture::commit(&ay, tag, None);
    let build: u64 = git(&ay, &["rev-list", "--count", "HEAD"])
        .parse()
        .expect("count");
    w.fx.release("ay", tag, &git(&ay, &["rev-parse", "HEAD"]), Release::GOOD);
    build
}

#[test]
fn a_failing_pack_retries_then_latches_and_a_deleted_latch_retries() {
    let w = World::settled("alab-latch");
    w.opts.lock().expect("opts").block_after = 2;
    let build = ay_release(&w, "v0.5.0");
    w.fx.packer.fail_on("ay", ARM);
    let (r, _) = w.run(false);
    let e = r.expect_err("a run whose only candidate failed refuses");
    assert!(
        e.to_string().contains("every candidate failed to pack"),
        "{e}"
    );
    let state = w.state();
    assert!(
        !state.blocked_file("ay", build).exists(),
        "no latch after one failure"
    );
    let (r, _) = w.run(false);
    assert!(r.is_err());
    assert!(state.blocked_file("ay", build).exists());
    let packs = w.fx.packer.legs().len();
    let (o, _) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with(&format!(
            "BLOCKED: v0.5.0 (build {build}) latched by BLOCKED-ay-{build}.md"
        )),
        "{}",
        rendered(&o, "ay")
    );
    assert_eq!(
        w.fx.packer.legs().len(),
        packs,
        "a latched build is not re-packed"
    );
    std::fs::remove_file(state.blocked_file("ay", build)).expect("delete the latch");
    w.fx.packer.heal();
    let (o, _) = ok(w.run(false));
    assert_eq!(o.pinned, ["ay"]);
    assert!(
        !state.attempts_file("ay", build).exists(),
        "a packed build's counter goes"
    );
}

#[test]
fn a_half_packed_release_is_never_pinned_and_converges_when_unlatched() {
    let w = World::settled("alab-half");
    w.opts.lock().expect("opts").block_after = 1;
    let build = ay_release(&w, "v0.6.0");
    w.fx.packer.fail_on("ay", X86);
    let (r, _) = w.run(false);
    assert!(r.is_err());
    let half = String::from_utf8(
        w.fx.staging_asset("ay", build, &format!("pkg-ay-{build}.toml"))
            .expect("the aarch64 leg uploaded"),
    )
    .expect("utf8");
    assert!(!half.contains("x86_64-apple-darwin"), "{half}");
    let (o, _) = ok(w.run(false));
    assert!(
        !rendered(&o, "ay").starts_with("INDEX"),
        "a half-packed release never reads INDEX"
    );
    assert!(rendered(&o, "ay").starts_with("BLOCKED"));
    assert_eq!(
        w.fx.indexer.count(),
        2,
        "a half-packed build is never pinned"
    );
    std::fs::remove_file(w.state().blocked_file("ay", build)).expect("unlatch");
    w.fx.packer.heal();
    let (o, _) = ok(w.run(false));
    assert!(
        o.pinned.contains(&"ay".to_string()),
        "the re-pack converges and publishes"
    );
    let full = String::from_utf8(
        w.fx.staging_asset("ay", build, &format!("pkg-ay-{build}.toml"))
            .expect("staged"),
    )
    .expect("utf8");
    assert!(full.contains("x86_64-apple-darwin") && full.contains("aarch64-apple-darwin"));
}

#[test]
fn a_one_shot_500_on_one_quad_asset_is_recovered() {
    let w = World::new("alab-500-quad");
    w.fx.host.fail_once(
        &w.fx.ctx.index_slug(),
        "atpkg-index-5",
        "aterm-machines.toml.sig",
    );
    let (o, _) = ok(w.run(true));
    assert_eq!(o.head, Some(5), "the retry finishes the partial download");
}

#[test]
fn a_staging_owner_that_cannot_be_read_is_never_read_as_absent() {
    let def = atpkg_publish::programs::get("ay").expect("ay");
    let facts = alab::Facts {
        program: def,
        pin: Some(2),
        release: Ok(alab::SignedRelease {
            tag: "v0.7.0".into(),
            public_commit: "ab".repeat(20),
            dev_commit: "cd".repeat(20),
            build: 9,
        }),
        staging: alab::Staging::Unknown("alabsystems unreachable".into()),
        latched: false,
    };
    assert_eq!(
        alab::decide(&facts),
        Decision::Skip("alabsystems unreachable".into()),
        "a transient failure never demotes a staged build to a re-pack"
    );
}

#[test]
fn an_unreachable_staging_owner_decides_nothing_and_a_retried_500_never_repacks() {
    let w = World::settled("alab-unreachable");
    let build = ay_release(&w, "v0.7.0");
    w.fx.stage("ay", build, &[ARM, X86]);
    let packs = w.fx.packer.legs().len();
    w.fx.host.set_unreachable("alabsystems", true);
    let (o, _) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with(
            "UNRESOLVED: git fetch failed in ~/.local/state/aterm-auto/alab/progs/ay: \
             unreachable: alabsystems did not answer"
        ),
        "{}",
        rendered(&o, "ay")
    );
    assert_eq!(
        w.fx.packer.legs().len(),
        packs,
        "unreachable is never 'absent'"
    );
    w.fx.host.set_unreachable("alabsystems", false);
    let slug = w.fx.ctx.staging_slug("ay");
    w.fx.host.fail_once(
        &slug,
        &format!("atpkg-ay-{build}"),
        &format!("pkg-ay-{build}.toml"),
    );
    let (o, _) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with(&format!("INDEX: v0.7.0 (build {build})")),
        "{}",
        rendered(&o, "ay")
    );
    assert_eq!(
        w.fx.packer.legs().len(),
        packs,
        "a retried 500 never re-packs"
    );
}

#[test]
fn what_is_built_is_what_was_signed() {
    let w = World::settled("alab-signed");
    let ay = w.dev("ay");
    fixture::commit(&ay, "v0.8.0", None);
    let sha = git(&ay, &["rev-parse", "HEAD"]);
    let packs = w.fx.packer.legs().len();
    w.fx.release(
        "ay",
        "v0.8.0",
        &sha,
        Release {
            tamper: true,
            ..Release::GOOD
        },
    );
    let (o, _) = ok(w.run(false));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unverified(why) if why.starts_with("SHA256SUMS.sig does not verify under the channel's roster")),
        "{:?}",
        row(&o, "ay").decision
    );
    w.fx.release(
        "ay",
        "v0.8.0",
        &sha,
        Release {
            header_tag: Some("v9.9.9"),
            ..Release::GOOD
        },
    );
    let (o, _) = ok(w.run(false));
    let public = &w.fx.ctx.accounts.public;
    assert!(
        rendered(&o, "ay").contains(&format!(
            "UNVERIFIED: the signed SHA256SUMS names {public}/ay v9.9.9, not {public}/ay v0.8.0"
        )),
        "{}",
        rendered(&o, "ay")
    );
    w.fx.host.put(
        &fixture::public(&w.fx, "ay"),
        "v0.8.0",
        atpkg_publish::host::fake::FakeRelease::default(),
    );
    w.fx.release(
        "ay",
        "v0.8.1",
        &sha,
        Release {
            ledger: false,
            ..Release::GOOD
        },
    );
    let (o, _) = ok(w.run(false));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why) if why.contains("has no ay row whose release_rev is the signed commit")),
        "{:?}",
        row(&o, "ay").decision
    );
    assert_eq!(
        w.fx.packer.legs().len(),
        packs,
        "nothing unverified or unbound is built"
    );
    w.fx.release("ay", "v0.8.2", &sha, Release::GOOD);
    let (o, _) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with("PACK: v0.8.2"),
        "{}",
        rendered(&o, "ay")
    );
}

fn gap_world(label: &str) -> World {
    static MADE: Made<(), Vec<String>> = Mutex::new(BTreeMap::new());
    let fx = Fx::new(label);
    let dir = fx.path("dev/trust");
    let trust = made_once(&MADE, &(), &dir, |dir| {
        fixture::repo(dir, 0);
        for i in 1..=9 {
            fixture::commit(dir, &format!("t{i}"), Some("2026-09-17"));
        }
        for i in 10..=12 {
            fixture::commit(dir, &format!("t{i}"), Some("2026-10-01"));
        }
        git(dir, &["rev-list", "--reverse", "HEAD"])
            .lines()
            .map(str::to_string)
            .collect()
    });
    fx.host
        .add_clone_source(&fx.ctx.staging_slug("trust"), &dir);
    fx.release("trust", "v0.9.0", at(&trust, 7), Release::GOOD);
    fx.publish_head(5, &[("trust", 9)]);
    let w = World {
        opts: Mutex::new(Options::new(&fx.ctx, false)),
        fx,
        pins: Mutex::new(BTreeMap::new()),
    };
    w.pins.lock().expect("pins").insert(
        ("trust".into(), 9),
        PinCommit {
            commit: at(&trust, 9)[..9].to_string(),
            date: Some("2026-09-17".into()),
        },
    );
    w
}

#[test]
fn the_gap_to_trust_main_is_counted_on_the_graph_and_told_once_a_day() {
    let w = gap_world("alab-gap");
    let (o, _) = ok(w.run(false));
    let rendered = rendered(&o, "trust");
    assert!(
        rendered.contains("3 builds behind trust main"),
        "{rendered}"
    );
    assert_eq!(
        w.fx.notes
            .shown()
            .iter()
            .filter(|n| n.contains("behind trust main"))
            .count(),
        1
    );
    let before = w.fx.notes.shown().len();
    ok(w.run(true));
    assert_eq!(w.fx.notes.shown().len(), before);
    ok(w.run(false));
    assert_eq!(
        w.fx.notes
            .shown()
            .iter()
            .filter(|n| n.contains("behind trust main"))
            .count(),
        1
    );
    let mut tomorrow = w.fx.ctx.clone();
    tomorrow.now_unix = fixture::later(1);
    ok(w.run_as(&tomorrow, false));
    assert_eq!(
        w.fx.notes
            .shown()
            .iter()
            .filter(|n| n.contains("behind trust main"))
            .count(),
        2
    );
}

#[test]
fn a_pin_off_main_is_said_to_be_off_it() {
    let w = gap_world("alab-gap-off");
    let trust = w.dev("trust");
    git(
        &trust,
        &["checkout", "-q", "-b", "side", &sha_at(&trust, 7)],
    );
    fixture::commit(&trust, "side8", Some("2026-09-17"));
    let side = fixture::commit(&trust, "side9", Some("2026-09-17"));
    git(&trust, &["checkout", "-q", "main"]);
    w.pins.lock().expect("pins").insert(
        ("trust".into(), 9),
        PinCommit {
            commit: side[..9].to_string(),
            date: Some("2026-09-17".into()),
        },
    );
    let (o, _) = ok(w.run(false));
    let rendered = rendered(&o, "trust");
    assert!(
        rendered.contains("off main, 5 commits behind trust main"),
        "{rendered}"
    );
}

#[test]
fn a_pin_whose_commit_the_clone_lacks_falls_back_to_build_numbers() {
    let w = gap_world("alab-gap-number");
    w.pins.lock().expect("pins").insert(
        ("trust".into(), 9),
        PinCommit {
            commit: "012345678".into(),
            date: Some("2026-09-17".into()),
        },
    );
    let (o, _) = ok(w.run(false));
    let rendered = rendered(&o, "trust");
    assert!(
        rendered.contains("3 builds behind trust main"),
        "{rendered}"
    );
}

#[test]
fn a_pin_at_main_owes_nothing() {
    let w = gap_world("alab-gap-at");
    let trust = w.dev("trust");
    w.fx.publish_head(6, &[("trust", 12)]);
    w.pins.lock().expect("pins").insert(
        ("trust".into(), 12),
        PinCommit {
            commit: sha_at(&trust, 12)[..9].to_string(),
            date: Some("2026-10-01".into()),
        },
    );
    ok(w.run(false));
    assert!(
        !w.fx
            .notes
            .shown()
            .iter()
            .any(|n| n.contains("behind trust main"))
    );
}

#[test]
fn a_run_in_flight_covers_this_one_and_a_stalled_one_is_refused() {
    let w = World::new("alab-lock");
    let state = w.state();
    state.ensure().expect("state");
    let holder = aterm_update_core::FileLock::acquire(&state.lock).expect("hold the lane lock");
    let (o, _) = ok(w.run(false));
    assert!(o.covered);
    state.mark_started(atpkg_publish::testkit::NOW - 13 * 3600);
    let (r, _) = w.run(false);
    let e = r.expect_err("a stalled run is refused");
    assert!(e.to_string().contains("a stalled run"), "{e}");
    assert!(w.fx.notes.shown().iter().any(|n| n.contains("stalled run")));
    drop(holder);
    let (o, _) = ok(w.run(true));
    assert!(!o.covered);
}

#[test]
fn a_finished_run_leaves_no_start_stamp_for_the_next_lock_holder_to_be_read_as() {
    let w = World::new("alab-run-ended");
    ok(w.run(false));
    let state = w.state();
    assert!(!state.started.exists(), "the run that stamped it ended");
    let holder = aterm_update_core::FileLock::acquire(&state.lock).expect("hold the lane lock");
    let mut later = w.fx.ctx.clone();
    later.now_unix = fixture::later(1);
    let (o, _) = ok(w.run_as(&later, false));
    assert!(o.covered);
    assert!(
        !w.fx.notes.shown().iter().any(|n| n.contains("stalled run")),
        "{:?}",
        w.fx.notes.shown()
    );
    drop(holder);
}

#[test]
fn a_signing_run_needs_the_key_and_both_token_files() {
    let w = World::new("alab-preflight");
    std::fs::remove_file(&w.fx.ctx.tokens.staging).expect("rm token");
    let (r, _) = w.run(false);
    let e = r.expect_err("no staging token");
    assert!(matches!(e, Error::CannotRun(_)), "{e}");
    assert!(
        e.to_string().contains("staging token file missing/empty"),
        "{e}"
    );
    ok(w.run(true));
}

#[test]
fn a_signing_run_needs_the_compressor_before_any_build() {
    let w = World::new("alab-compressor");
    let lacks = atpkg_publish::artifact::tarwrite::compressor_in(Some(std::ffi::OsStr::new("")))
        .expect_err("an empty PATH has no zstd");
    *w.fx.packer.lacks.lock().expect("lacks") = Some(lacks);
    let (r, _) = w.run(false);
    let e = r.expect_err("no compressor");
    assert!(matches!(e, Error::CannotRun(_)), "{e}");
    assert!(
        e.to_string()
            .contains("this Mac cannot pack: no `zstd` on this builder's PATH"),
        "{e}"
    );
    assert!(
        w.fx.packer.legs().is_empty(),
        "a build ran without a compressor"
    );
    assert!(
        !w.state().attempts_file("ay", 3).exists(),
        "an attempt was counted"
    );
    ok(w.run(true));

    *w.fx.packer.lacks.lock().expect("lacks") = None;
    let (o, _) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with("PACK: v0.2.0 (build 3)"),
        "{}",
        rendered(&o, "ay")
    );
    assert!(!w.fx.packer.legs().is_empty(), "the control packed nothing");
}

#[test]
fn the_newest_signed_release_is_the_newest_published_vxyz_carrying_its_signature() {
    let rel = |tag: &str, draft: bool, pre: bool, signed: bool| atpkg_publish::host::Release {
        tag: tag.into(),
        draft,
        prerelease: pre,
        assets: if signed {
            vec!["SHA256SUMS".into(), "SHA256SUMS.sig".into()]
        } else {
            vec!["SHA256SUMS".into()]
        },
    };
    let releases = [
        rel("v0.9.0", false, false, true),
        rel("v0.10.0", false, false, true),
        rel("v0.11.0", true, false, true),
        rel("v0.12.0", false, true, true),
        rel("v0.13.0", false, false, false),
        rel("v1.0", false, false, true),
        rel("atpkg-index-4", false, false, true),
    ];
    assert_eq!(
        alab::newest_signed_tag(&releases).as_deref(),
        Some("v0.10.0")
    );
    assert_eq!(alab::newest_signed_tag(&[]), None);
}

#[test]
fn a_compilers_version_names_the_pins_commit_and_date() {
    let vv = "trustc 1.99.0-dev\nbinary: rustc\ncommit-hash: 321aaeda7f00ba\ncommit-date: 2026-09-17\nhost: aarch64-apple-darwin\n";
    assert_eq!(
        alab::parse_vv(vv),
        Some(PinCommit {
            commit: "321aaeda7".into(),
            date: Some("2026-09-17".into())
        })
    );
    assert_eq!(alab::parse_vv("commit-hash: unknown\n"), None);
}

#[test]
fn the_bundle_lane_prints_the_bundle_packs_never_runs_them() {
    let w = World::new("alab-bundle");
    let mc = w.dev("trust-mc");
    fixture::commit(&mc, "v0.7.0", None);
    w.fx.release(
        "trust-mc",
        "v0.7.0",
        &git(&mc, &["rev-parse", "HEAD"]),
        Release::GOOD,
    );
    w.fx.stage("trust-mc", 4, &[ARM, X86]);
    let (o, _) = ok(w.run(false));
    let mc_row = row(&o, "trust-mc");
    assert!(matches!(mc_row.decision, Decision::BundleLane { .. }));
    let cmd = alab::bundle_command(mc_row);
    let legs: Vec<&String> = cmd
        .iter()
        .filter(|l| l.starts_with("atpkg-publish pack-bundle trust-mc "))
        .collect();
    assert_eq!(legs.len(), 2, "{cmd:#?}");
    assert!(legs[0].contains("--triple aarch64-apple-darwin") && !legs[0].contains("--merge-from"));
    assert!(legs[1].contains("--triple x86_64-apple-darwin --merge-from <out dir>/pack-trust-mc-5/merge-base-trust-mc-5.toml"));
    let copy = cmd
        .iter()
        .position(|l| l.starts_with("cp -f <out dir>/pack-trust-mc-5/pkg-trust-mc-5.toml"))
        .expect("the base is copied aside");
    let first = cmd.iter().position(|l| *l == *legs[0]).expect("first");
    let second = cmd.iter().position(|l| *l == *legs[1]).expect("second");
    assert!(first < copy && copy < second, "{cmd:#?}");
    assert!(
        legs.iter()
            .all(|l| l.contains("--hello-sysroot <the Trust sysroot this leg's trust-mc-compiler"))
    );
    assert!(!cmd.iter().any(|l| l.contains("--no-hello")));
    let trust = alab::bundle_command(row(&o, "trust"));
    assert!(
        trust.iter().any(|l| l
            == "atpkg-publish publish-group rustc --seal <the staged copy of the seal> --upload")
    );
    assert!(
        !trust.iter().any(|l| l.contains("pack-bundle")),
        "{trust:#?}"
    );
}

#[test]
fn a_pins_record_from_other_targets_is_retired_once_never_believed() {
    let w = World::new("alab-restamp");
    let state = w.state();
    state.ensure().expect("state");
    std::fs::write(
        &state.pins_seen,
        "# pins-resolved-for aarch64-apple-darwin\nay 99\n",
    )
    .expect("write");
    let (_, _) = ok(w.run(false));
    let text = std::fs::read_to_string(&state.pins_seen).expect("re-stamped");
    assert!(
        text.starts_with("# pins-resolved-for aarch64-apple-darwin x86_64-apple-darwin\n"),
        "{text}"
    );
}

#[test]
fn a_pinned_program_with_no_signed_release_is_skipped_and_holds_nothing_else() {
    let w = World::new("alab-no-release");
    let text = w.fx.index_text(
        6,
        &[
            ("ay", 2),
            ("clean", 4),
            ("nn", 2),
            ("trust-ir", 1),
            ("trust", 5),
            ("trust-mc", 4),
        ],
    );
    w.fx.publish_index_text(6, &text);
    let (o, _) = ok(w.run(false));
    let public = &w.fx.ctx.accounts.public;
    assert!(
        rendered(&o, "clean").starts_with(&format!(
            "SKIP: no signed vX.Y.Z release readable on {public}/clean"
        )),
        "{}",
        rendered(&o, "clean")
    );
    assert_eq!(
        o.pinned,
        ["ay"],
        "a skipped program in no group holds nothing"
    );
}

#[test]
fn a_staged_pack_that_does_not_verify_is_never_pinned_or_packed_over() {
    let w = World::settled("alab-invalid-stage");
    let ay = w.dev("ay");
    fixture::commit(&ay, "v0.9.0", None);
    w.fx.release(
        "ay",
        "v0.9.0",
        &git(&ay, &["rev-parse", "HEAD"]),
        Release::GOOD,
    );
    let slug = w.fx.ctx.staging_slug("ay");
    let text = fixture::manifest("ay", 5, &[ARM, X86]);
    w.fx.host
        .put_asset(&slug, "atpkg-ay-5", "pkg-ay-5.toml", text.into_bytes());
    w.fx.host
        .put_asset(&slug, "atpkg-ay-5", "pkg-ay-5.toml.sig", vec![0u8; 64]);
    let packs = w.fx.packer.legs().len();
    let (o, _) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with("SKIP: atpkg-ay-5 on alabsystems does not verify"),
        "{}",
        rendered(&o, "ay")
    );
    assert_eq!(w.fx.packer.legs().len(), packs);
    assert_eq!(w.fx.indexer.count(), 2);
}

#[test]
fn a_horizon_inside_thirty_days_is_told() {
    let w = World::new("alab-horizon");
    let text =
        w.fx.index_text(6, &[("nn", 2)])
            .replace(atpkg_publish::testkit::FAR_FUTURE, "2026-11-01T00:00:00Z");
    w.fx.publish_index_text(6, &text);
    ok(w.run(false));
    let shown = w.fx.notes.shown();
    assert!(
        shown.iter().any(|n| n.contains("2026-11-01T00:00:00Z")),
        "{shown:?}"
    );
}

fn head_with_group(w: &World, n: u64, program: &str, group: &str) {
    let pins = [
        ("ay", 2),
        ("nn", 2),
        ("trust-ir", 1),
        ("trust", 5),
        ("trust-mc", 4),
        ("claude", 2_026_090_101),
    ];
    let repo = atpkg_publish::programs::get(program).map_or(program, |d| d.repo);
    let block = format!("[programs.{program}]\nrepo = \"{repo}\"\npolicy = \"prebuilt-only\"\n");
    let text = w.fx.index_text(n, &pins);
    w.fx.publish_index_text(
        n,
        &text.replace(&block, &format!("{block}coherence_group = \"{group}\"\n")),
    );
}

#[test]
fn the_hold_follows_the_group_the_channel_enforces() {
    let w = World::new("alab-head-group");
    head_with_group(&w, 6, "ay", "rustc");
    let (o, _) = ok(w.run(false));
    assert!(o.packed.contains(&"ay".to_string()), "{:?}", o.packed);
    assert_eq!(o.held, ["ay", "trust-ir"]);
    assert!(o.published.is_none(), "nothing may move while trust waits");
}

#[test]
fn a_head_and_table_that_disagree_on_a_group_refuse_the_run() {
    let w = World::new("alab-group-divergence");
    head_with_group(&w, 6, "trust-ir", "other");
    let (r, _) = w.run(true);
    let e = r.expect_err("divergence");
    assert!(
        e.to_string().contains(
            "index-6 puts trust-ir in coherence group `other`, but this build's program table \
             says `rustc`"
        ),
        "{e}"
    );
}

#[test]
fn a_scratch_clone_is_fetched_as_the_staging_identity_never_by_its_origin() {
    let w = World::settled("alab-fetch");
    let clone = w.state().dir.join("progs/ay");
    git(&clone, &["remote", "set-url", "origin", "/nonexistent/ay"]);
    let build = ay_release(&w, "v0.7.0");
    let (o, tx) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with(&format!("PACK: v0.7.0 (build {build})")),
        "{}",
        tx.text()
    );
    assert!(
        w.fx.calls("fetch-repo ")
            .contains(&"fetch-repo alabsystems/ay".to_string())
    );
}

#[test]
fn a_sums_larger_than_a_manifest_is_still_read_and_verified() {
    let w = World::settled("alab-big-sums");
    let ay = w.dev("ay");
    fixture::commit(&ay, "v0.7.0", None);
    let build: u64 = git(&ay, &["rev-list", "--count", "HEAD"])
        .parse()
        .expect("count");
    let listing = usize::try_from(atpkg::net::MANIFEST_CAP / 64).expect("small");
    w.fx.release(
        "ay",
        "v0.7.0",
        &git(&ay, &["rev-parse", "HEAD"]),
        Release {
            listing,
            ..Release::GOOD
        },
    );
    let sums =
        w.fx.host
            .asset(&w.fx.ctx.public_slug("ay"), "v0.7.0", "SHA256SUMS")
            .expect("published");
    assert!(u64::try_from(sums.len()).expect("len") > atpkg::net::MANIFEST_CAP);
    let (o, tx) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with(&format!("PACK: v0.7.0 (build {build})")),
        "{}",
        tx.text()
    );
}

#[test]
fn a_ledger_key_that_is_not_a_full_sha_never_reaches_git() {
    let w = World::settled("alab-ledger-key");
    let ay = w.dev("ay");
    fixture::commit(&ay, "v0.7.0", None);
    w.fx.release("ay", "v0.7.0", "--upload-pack=touch", Release::GOOD);
    let (o, _) = ok(w.run(true));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why) if why.contains("`--upload-pack=touch`, which is not a 40-hex dev commit")),
        "{:?}",
        row(&o, "ay").decision
    );
}

#[test]
fn a_key_the_roster_does_not_list_packs_and_signs_nothing() {
    let w = World::new("alab-unrostered");
    let (stranger, stranger_pub) = atpkg_keys::generate().expect("a key no roster lists");
    std::fs::write(&w.fx.ctx.identity.key, stranger).expect("key");
    std::fs::write(
        &w.fx.ctx.identity.record,
        format!(
            "id = \"m99\"\npubkey = \"{stranger_pub}\"\nminted_at = \"2026-08-04T00:00:00Z\"\n"
        ),
    )
    .expect("record");
    let (r, _) = w.run(false);
    let e = r.expect_err("an unrostered key refuses");
    assert!(
        e.to_string().contains("machine 'm99' is NOT on the roster"),
        "{e}"
    );
    assert!(w.fx.packer.legs().is_empty() && w.fx.indexer.count() == 0);
    assert!(w.fx.calls("clone ").is_empty(), "refused before any clone");
    ok(w.run(true));
}
