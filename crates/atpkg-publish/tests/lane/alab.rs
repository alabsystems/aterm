// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! End-to-end Apple lane tests over the fake channel and real git.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use atpkg_publish::api::{Build, Delta};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::{Error, Result};
use atpkg_publish::lane::LaneKind;
use atpkg_publish::lane::alab::{self, Decision, Deps, Options, Outcome, Row};
use atpkg_publish::lane::gap::PinCommit;
use atpkg_publish::lane::report::Transcript;
use atpkg_publish::lane::state::LaneState;
use atpkg_publish::triple::{AARCH64_APPLE_DARWIN as ARM, Triple, X86_64_APPLE_DARWIN as X86};

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
    assert!(
        w.fx.notes.entries().is_empty(),
        "a dry run records no notices"
    );
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
    for h in [
        "claude-code-alab",
        "muse-alab",
        "agy-alab",
        "grok-alab",
        "kimi-alab",
    ] {
        assert!(
            rendered(&o, h).starts_with("SKIP: not pinned by index-5"),
            "a harness row is inert until a person pins it: {}",
            rendered(&o, h)
        );
    }
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
        w.fx.notes.entries().is_empty(),
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
        public_on: std::collections::BTreeSet::new(),
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
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why) if why.contains("has no live ay row whose release_rev is the signed commit")),
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
            .entries()
            .iter()
            .filter(|n| n.contains("behind trust main"))
            .count(),
        1
    );
    let before = w.fx.notes.entries().len();
    ok(w.run(true));
    assert_eq!(w.fx.notes.entries().len(), before);
    ok(w.run(false));
    assert_eq!(
        w.fx.notes
            .entries()
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
            .entries()
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
            .entries()
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
    assert!(
        w.fx.notes
            .entries()
            .iter()
            .any(|n| n.contains("stalled run"))
    );
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
        !w.fx
            .notes
            .entries()
            .iter()
            .any(|n| n.contains("stalled run")),
        "{:?}",
        w.fx.notes.entries()
    );
    drop(holder);
}

#[test]
fn a_signing_run_needs_the_key_and_both_token_files() {
    // The `gh` arm's token check, said outright; under agh both writers are asked instead
    // (`under_agh::a_signing_run_asks_both_writers_under_agh`).
    let mut w = World::new("alab-preflight");
    w.fx.use_gh();
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
fn a_signing_run_needs_a_packer_that_can_pack_before_any_build() {
    let w = World::new("alab-can-pack");
    *w.fx.packer.lacks.lock().expect("lacks") = Some("a stand-in packer's lack".into());
    let (r, _) = w.run(false);
    let e = r.expect_err("a packer that cannot pack");
    assert!(matches!(e, Error::CannotRun(_)), "{e}");
    assert!(
        e.to_string()
            .contains("this Mac cannot pack: a stand-in packer's lack"),
        "{e}"
    );
    assert!(
        w.fx.packer.legs().is_empty(),
        "a build ran on a packer that cannot pack"
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
        published_at: None,
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
    let date_of = |date: &str| {
        alab::parse_vv(&format!(
            "commit-hash: 321aaeda7f00ba\ncommit-date: {date}\n"
        ))
        .expect("the commit reads")
        .date
    };
    assert_eq!(
        date_of("2026-09-17 extra"),
        Some("2026-09-17".into()),
        "the first ten bytes, where they are a date"
    );
    assert_eq!(
        date_of("2026-09-1\u{e9}extra"),
        None,
        "byte 10 inside a two-byte character is no date and no panic"
    );
    assert_eq!(
        date_of("2026-09-\u{e9}"),
        None,
        "ten bytes that are no date"
    );
    assert_eq!(date_of("unknown"), None);
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
            == "atpkg-publish publish-group rustc --seal <the staged copy of the seal> --stage-upload")
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
    let horizon_secs = atpkg_publish::lane::alab::HORIZON_WARN_DAYS * 86_400;
    let at_boundary = aterm_types::rfc3339::format_rfc3339(
        u64::try_from(atpkg_publish::testkit::NOW + horizon_secs).expect("non-negative"),
    );
    let w = World::new("alab-horizon-30d");
    let text =
        w.fx.index_text(6, &[("nn", 2)])
            .replace(atpkg_publish::testkit::FAR_FUTURE, &at_boundary);
    w.fx.publish_index_text(6, &text);
    ok(w.run(false));
    assert!(
        !w.fx
            .notes
            .entries()
            .iter()
            .any(|n| n.contains(&at_boundary)),
        "exact 30-day boundary does not warn: {:?}",
        w.fx.notes.entries()
    );

    let inside = aterm_types::rfc3339::format_rfc3339(
        u64::try_from(atpkg_publish::testkit::NOW + horizon_secs - 1).expect("non-negative"),
    );
    let w = World::new("alab-horizon-under-30d");
    let text =
        w.fx.index_text(6, &[("nn", 2)])
            .replace(atpkg_publish::testkit::FAR_FUTURE, &inside);
    w.fx.publish_index_text(6, &text);
    ok(w.run(false));
    let shown = w.fx.notes.entries();
    assert!(shown.iter().any(|n| n.contains(&inside)), "{shown:?}");
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

/// A home `~/publication` whose ledger maps `repo`'s `dev` commit to `tag`'s signed commit:
/// the checkout the lane read before, which no release fast-forwards any more.
fn home_ledger(w: &World, repo: &str, dev: &str, tag: &str) {
    let home = w.fx.ctx.home.join("publication");
    std::fs::create_dir_all(&home).expect("mkdir");
    let public = fixture::hex40(&format!("{repo}@{tag}"));
    std::fs::write(
        home.join("mappings.json"),
        format!(
            "{{\"mappings\": {{\"{repo}\": {{\"{dev}\": {{\"release_rev\": \"{public}\"}}}}}}}}"
        ),
    )
    .expect("a home checkout's ledger");
}

/// THE LEDGER IS THE ENGINE ORIGIN'S MAIN, READ AS THE STAGING IDENTITY, NEVER AN ENGINE
/// CHECKOUT ON THIS MAC: a home `~/publication` that a release no longer fast-forwards maps the
/// new release to another dev commit (the one before), while the engine's main (where the
/// promote recorded it) maps it to the commit it was cut from, and that one is built. NEGATIVE
/// CONTROL: the lane's old read of `~/publication/mappings.json` built the build before; a read
/// of both found two live rows and built none.
#[test]
fn the_ledger_is_read_from_the_engine_origins_main_never_a_home_checkout() {
    let w = World::settled("alab-engine-main");
    let build = ay_release(&w, "v0.7.0");
    let stale = git(&w.dev("ay"), &["rev-parse", "HEAD~1"]);
    home_ledger(&w, "ay", &stale, "v0.7.0");
    let (o, tx) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with(&format!("PACK: v0.7.0 (build {build})")),
        "{}",
        tx.text()
    );
    assert!(
        w.fx.calls("fetch-main ")
            .contains(&"fetch-main alabsystems/publication".to_string()),
        "{:?}",
        w.fx.calls("fetch-main ")
    );
}

/// THE ENGINE'S MAIN IS READ THROUGH THE STAGING HOST, WHOSE OWNER HOLDS THE ENGINE: with a
/// channel host that reads no repository's main, the release is still mapped and built.
/// NEGATIVE CONTROL: a read through the channel host (the public owner's identity, which cannot
/// see the private engine) is refused, and the release is UNRESOLVED.
#[test]
fn the_engines_main_is_read_through_the_staging_host() {
    let w = World::settled("alab-engine-staging-host");
    let build = ay_release(&w, "v0.7.0");
    let mut ctx = w.fx.ctx.clone();
    ctx.hosts = atpkg_publish::host::Hosts {
        staging: w.fx.host.clone(),
        channel: std::sync::Arc::new(under_agh::RefusesOne {
            inner: w.fx.host.clone(),
            refused: ctx.staging_slug("never-asked"),
        }),
    };
    let (o, tx) = ok(w.run_as(&ctx, false));
    assert!(
        rendered(&o, "ay").starts_with(&format!("PACK: v0.7.0 (build {build})")),
        "{}",
        tx.text()
    );
}

/// A LEDGER THE ENGINE'S MAIN DOES NOT HOLD, OR HOLDS UNREADABLE, RESOLVES NOTHING: with
/// `mappings.json` removed from the engine's main, and again with it unparseable, the release
/// is UNRESOLVED with that reason and nothing is packed. NEGATIVE CONTROL: either read as an
/// empty ledger would say the promote recorded no row yet.
#[test]
fn a_ledger_the_engines_main_lacks_or_garbles_resolves_nothing() {
    let w = World::settled("alab-engine-no-ledger");
    ay_release(&w, "v0.7.0");
    let packs = w.fx.packer.legs().len();
    let origin = w.fx.engine_origin();
    git(&origin, &["rm", "-q", "mappings.json"]);
    fixture::commit(&origin, "no ledger", None);
    let (o, _) = ok(w.run(false));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why)
            if why.contains("alabsystems/publication's main has no readable mappings.json")),
        "{:?}",
        row(&o, "ay").decision
    );
    std::fs::write(origin.join("mappings.json"), "{not json").expect("garble");
    git(&origin, &["add", "mappings.json"]);
    fixture::commit(&origin, "garbled ledger", None);
    let (o, _) = ok(w.run(false));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why)
            if why.contains("the ledger on alabsystems/publication main")
                && why.contains("does not parse")),
        "{:?}",
        row(&o, "ay").decision
    );
    assert_eq!(
        w.fx.packer.legs().len(),
        packs,
        "nothing unresolved is built"
    );
}

/// A REVOKED ROW NEVER NAMES THE DEV COMMIT A SIGNED RELEASE IS BUILT FROM, END TO END: the
/// release's signed commit is the `release_rev` of two rows, as ay's and trust-ir's were on
/// 2026-11-07's ledger. While both are live the lane decides neither (NEGATIVE CONTROL: the
/// lane before 54cf70906b took the first row, revoked or not); once the engine revokes one, the
/// other is built.
#[test]
fn a_revoked_row_never_names_the_dev_commit_a_release_is_built_from() {
    let w = World::settled("alab-revoked-row");
    let build = ay_release(&w, "v0.7.0");
    let ay = w.dev("ay");
    let live = git(&ay, &["rev-parse", "HEAD"]);
    let revoked = git(&ay, &["rev-parse", "HEAD~1"]);
    w.fx.map("ay", &revoked, "v0.7.0");
    let (o, _) = ok(w.run(false));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why) if why.contains("to 2 live ay rows")),
        "{:?}",
        row(&o, "ay").decision
    );
    let packs = w.fx.packer.legs().len();
    w.fx.revoke("ay", &revoked);
    let (o, tx) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with(&format!("PACK: v0.7.0 (build {build})")),
        "{}",
        tx.text()
    );
    assert!(w.fx.packer.legs().len() > packs, "the live row is built");
    assert_ne!(live, revoked);
}

/// AN ENGINE MAIN THE HOST DOES NOT SERVE RESOLVES NOTHING: the release is UNRESOLVED with the
/// host's reason and nothing is packed, though a home `~/publication` maps it to the commit it
/// was cut from. NEGATIVE CONTROL: a lane that falls back to that checkout when the engine's
/// main cannot be read finds the live row there and packs v0.7.0.
#[test]
fn an_engine_main_the_host_does_not_serve_resolves_nothing() {
    let w = World::settled("alab-engine-unread");
    ay_release(&w, "v0.7.0");
    let head = git(&w.dev("ay"), &["rev-parse", "HEAD"]);
    home_ledger(&w, "ay", &head, "v0.7.0");
    let packs = w.fx.packer.legs().len();
    w.fx.host
        .set_not_visible(&w.fx.ctx.staging_slug("publication"), true);
    let (o, _) = ok(w.run(false));
    assert!(
        matches!(&row(&o, "ay").decision, Decision::Unresolved(why)
            if why.contains("could not read alabsystems/publication's main from the host")
                && why.contains("not visible to this identity (HTTP 404)")),
        "{:?}",
        row(&o, "ay").decision
    );
    assert_eq!(
        w.fx.packer.legs().len(),
        packs,
        "nothing unresolved is built"
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

/// Site 5 of finding F2 (plan §15): under agh the signing run asks each owner's writer, the
/// channel's first, and a dry run asks neither.
mod under_agh {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use atpkg_publish::error::Error;
    use atpkg_publish::host::fake::{FakeFail, FakeHost, FakeOp};
    use atpkg_publish::host::{HostError, Hosts, Probe, Release, ReleaseHost, Replace, Slug};

    use super::{World, ok};

    /// A `FakeHost` whose `write_preflight` refuses one slug, a program repository's, and
    /// delegates every other: the run's staging host only, so the role, not the slug, says
    /// which check asks. It reads no repository's main (`fetch_main` is the trait's refusing
    /// default), so as a run's channel host it shows which host the engine's main is read on.
    pub(super) struct RefusesOne {
        pub(super) inner: Arc<FakeHost>,
        pub(super) refused: Slug,
    }

    impl ReleaseHost for RefusesOne {
        fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
            self.inner.releases(slug)
        }
        fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
            self.inner.release(slug, tag)
        }
        fn download(
            &self,
            slug: &Slug,
            tag: &str,
            assets: &[&str],
            dir: &Path,
        ) -> Result<(), HostError> {
            self.inner.download(slug, tag, assets, dir)
        }
        fn create(
            &self,
            slug: &Slug,
            tag: &str,
            title: &str,
            notes: &str,
            files: &[PathBuf],
        ) -> Result<(), HostError> {
            self.inner.create(slug, tag, title, notes, files)
        }
        fn create_draft(
            &self,
            slug: &Slug,
            tag: &str,
            title: &str,
            notes: &str,
            files: &[PathBuf],
        ) -> Result<(), HostError> {
            self.inner.create_draft(slug, tag, title, notes, files)
        }
        fn upload(
            &self,
            slug: &Slug,
            tag: &str,
            files: &[PathBuf],
            replace: Replace,
        ) -> Result<(), HostError> {
            self.inner.upload(slug, tag, files, replace)
        }
        fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
            self.inner.publish_draft(slug, tag)
        }
        fn write_preflight(&self, slug: &Slug) -> Result<(), HostError> {
            if *slug == self.refused {
                Err(HostError::Refused("this writer is refused".into()))
            } else {
                self.inner.write_preflight(slug)
            }
        }
        fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
            self.inner.probe_public(slug, tag, asset)
        }
        fn fetch_public(
            &self,
            slug: &Slug,
            tag: &str,
            asset: &str,
            dest: &Path,
            cap: u64,
        ) -> Probe {
            self.inner.fetch_public(slug, tag, asset, dest, cap)
        }
        fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
            self.inner.clone_repo(slug, dest)
        }
        fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
            self.inner.fetch_repo(slug, dir)
        }
    }

    fn no_write(host: &FakeHost) {
        let calls = host.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("create")
                || c.starts_with("upload")
                || c.starts_with("publish-draft")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_signing_run_asks_both_writers_under_agh() {
        let w = World::new("alab-writers");
        let mut ctx = w.fx.ctx.clone();
        let channel = ctx.index_slug();
        let staging = ctx.staging_slug(atpkg::manifest::INDEX_REPO);

        w.fx.host.fail_next(FakeOp::Preflight, FakeFail::Refuse);
        let e = w.run_as(&ctx, false).0.expect_err("no channel writer");
        assert!(matches!(e, Error::CannotRun(_)), "{e}");
        let why = e.to_string();
        assert!(
            why.contains(&format!("channel writer unavailable on {channel}")),
            "{why}"
        );
        assert!(why.contains("refused by the test"), "{why}");
        let calls = w.fx.host.calls();
        assert!(calls.contains(&format!("preflight {channel}")), "{calls:?}");
        no_write(&w.fx.host);
        assert!(w.fx.packer.legs().is_empty(), "a build ran");

        // A tracked program's staging repository is asked too, after the index repository's:
        // the lane clones and stages it, and under agh push and a grant are per repository.
        let trust = ctx.staging_slug("trust");
        ctx.hosts = Hosts {
            staging: Arc::new(RefusesOne {
                inner: w.fx.host.clone(),
                refused: trust.clone(),
            }),
            channel: w.fx.host.clone(),
        };
        let e = w.run_as(&ctx, false).0.expect_err("no staging writer");
        assert!(matches!(e, Error::CannotRun(_)), "{e}");
        let why = e.to_string();
        assert!(
            why.contains(&format!("staging writer unavailable on {trust}")),
            "{why}"
        );
        assert!(why.contains("this writer is refused"), "{why}");
        let calls = w.fx.host.calls();
        assert!(calls.contains(&format!("preflight {staging}")), "{calls:?}");
        let public_trust = ctx.public_slug("trust");
        assert!(
            calls.contains(&format!("preflight {public_trust}")),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("clone ")),
            "refused before any clone: {calls:?}"
        );
        no_write(&w.fx.host);
        assert!(w.fx.packer.legs().is_empty(), "a build ran");

        let asked = |host: &FakeHost| {
            host.calls()
                .iter()
                .filter(|c| c.starts_with("preflight "))
                .count()
        };
        let before = asked(&w.fx.host);
        ok(w.run_as(&ctx, true));
        assert_eq!(asked(&w.fx.host), before, "a dry run asks no writer");
    }
}

/// Index `n` over [`World::new`]'s pins with ay's `pin` at `ay`, and `overlay` added as one
/// Apple target's `pin_by_target` cells.
fn head_with_overlay(w: &World, n: u64, ay: u64, target: Triple, overlay: &[(&str, u64)]) {
    let mut text = w.fx.index_text(
        n,
        &[
            ("ay", ay),
            ("nn", 2),
            ("trust-ir", 1),
            ("trust", 5),
            ("trust-mc", 4),
            ("claude", 2_026_090_101),
        ],
    );
    text.push_str(&format!("\n[channels.pin_by_target.{target}]\n"));
    for (p, b) in overlay {
        text.push_str(&format!("{p} = {b}\n"));
    }
    w.fx.publish_index_text(n, &text);
}

fn last_delta(w: &World) -> Delta {
    w.fx.indexer
        .requests
        .lock()
        .expect("requests")
        .last()
        .expect("an index was asked for")
        .delta
        .clone()
}

#[test]
fn an_apple_silicon_overlay_ahead_of_the_release_is_normal_and_the_release_moves_the_rest() {
    let w = World::new("alab-overlay-ahead");
    head_with_overlay(&w, 6, 2, ARM, &[("ay", 9)]);
    let (o, tx) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with("PACK: v0.2.0 (build 3) > pin 2"),
        "the lowest target decides: {}",
        rendered(&o, "ay")
    );
    assert_eq!(o.published, Some(7), "{}", tx.text());
    let delta = last_delta(&w);
    assert_eq!(delta.pins, BTreeMap::from([("ay".to_string(), 3)]));
    assert!(
        delta.retire_target.is_empty() && delta.target_pins.is_empty(),
        "the overlay ahead is carried untouched: {delta:?}"
    );
    let floors = std::fs::read_to_string(&w.state().pins_seen).expect("floors");
    for line in [
        "ay 3",
        "ay@aarch64-apple-darwin 9",
        "ay@x86_64-apple-darwin 3",
    ] {
        assert!(floors.lines().any(|l| l == line), "{line}: {floors}");
    }
}

#[test]
fn an_overlay_the_release_passes_is_retired_with_the_pin_it_follows() {
    let w = World::new("alab-overlay-behind");
    head_with_overlay(&w, 6, 2, X86, &[("ay", 1)]);
    let (o, _) = ok(w.run(false));
    assert!(
        rendered(&o, "ay").starts_with("PACK: v0.2.0 (build 3) > pin 1"),
        "{}",
        rendered(&o, "ay")
    );
    let delta = last_delta(&w);
    assert_eq!(delta.pins, BTreeMap::from([("ay".to_string(), 3)]));
    assert_eq!(
        delta.retire_target.into_iter().collect::<Vec<_>>(),
        [(X86, Some("ay".to_string()))]
    );
}

#[test]
fn a_release_already_public_on_one_target_is_never_repacked_for_the_other() {
    let w = World::new("alab-overlay-public");
    head_with_overlay(&w, 6, 2, ARM, &[("ay", 3)]);
    let (o, _) = ok(w.run(false));
    let ay = rendered(&o, "ay");
    assert!(
        ay.starts_with("SKIP: build 3 is already public on aarch64-apple-darwin"),
        "{ay}"
    );
    assert!(
        !w.fx.packer.legs().iter().any(|(p, _, _)| p == "ay"),
        "a published pack never gains rows: {:?}",
        w.fx.packer.legs()
    );
}

#[test]
fn a_lowered_overlay_is_a_pin_downgrade() {
    let w = World::new("alab-overlay-down");
    head_with_overlay(&w, 6, 2, ARM, &[("ay", 9)]);
    ok(w.run(false));
    head_with_overlay(&w, 8, 3, ARM, &[("ay", 8)]);
    let (r, _) = w.run(false);
    let e = r.expect_err("a lowered overlay refuses");
    assert!(
        e.to_string()
            .contains("PIN DOWNGRADE on the channel: ay@aarch64-apple-darwin 9 -> 8"),
        "{e}"
    );
}

#[test]
fn a_pin_ahead_of_the_build_raises_only_the_overlays_below_it() {
    let channel = atpkg::Channel {
        name: "stable".into(),
        channel_build: 1,
        min_build: 0,
        min_build_by_program: BTreeMap::new(),
        yanked: Vec::new(),
        pin: BTreeMap::from([("ay".to_string(), 10)]),
        pin_by_target: BTreeMap::from([
            (
                X86.as_str().to_string(),
                BTreeMap::from([("ay".to_string(), 4)]),
            ),
            (
                ARM.as_str().to_string(),
                BTreeMap::from([("ay".to_string(), 7)]),
            ),
        ]),
    };
    let mut delta = Delta::default();
    alab::move_pin(&channel, &mut delta, "ay", 6);
    assert!(delta.pins.is_empty(), "pin 10 is ahead of 6: {delta:?}");
    assert!(delta.retire_target.is_empty(), "{delta:?}");
    assert_eq!(
        delta.target_pins,
        BTreeMap::from([(X86, BTreeMap::from([("ay".to_string(), 6)]))])
    );
}
