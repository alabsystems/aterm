// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! End-to-end Linux lane publication, row state, and lineage gate tests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use atpkg_publish::artifact::lineage_switch::Switch;
use atpkg_publish::artifact::upload::VERSION_RESERVATION;
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Result;
use atpkg_publish::lane::LaneKind;
use atpkg_publish::lane::linux::{
    self, Builder, Decision, Deps, Options, Outcome, RowState, row_state,
};
use atpkg_publish::lane::report::Transcript;
use atpkg_publish::lane::state::LaneState;
use atpkg_publish::triple::{
    AARCH64_APPLE_DARWIN, AARCH64_UNKNOWN_LINUX_GNU as ARM, Triple, X86_64_APPLE_DARWIN,
    X86_64_UNKNOWN_LINUX_GNU as X86,
};

use super::super::shared::{Made, made_once, scratch};
use super::fixture::{Fx, Held, manifest, row};

struct FakeBuilder(Mutex<BTreeSet<Triple>>);

impl FakeBuilder {
    fn can(ts: &[Triple]) -> Self {
        Self(Mutex::new(ts.iter().copied().collect()))
    }
}

impl Builder for FakeBuilder {
    fn can_build(&self, t: Triple) -> std::result::Result<(), String> {
        if self.0.lock().expect("can").contains(&t) {
            Ok(())
        } else {
            Err(format!("no cross linker for {t} on this builder"))
        }
    }

    fn env_for(&self, t: Triple) -> std::result::Result<Vec<(String, String)>, String> {
        Ok(vec![(
            format!(
                "CARGO_TARGET_{}_LINKER",
                t.as_str().replace('-', "_").to_uppercase()
            ),
            format!(
                "{}-linux-gnu-gcc",
                t.as_str().split('-').next().unwrap_or("")
            ),
        )])
    }
}

struct World {
    fx: Fx,
    host: Triple,
    builder: FakeBuilder,
    switch: Mutex<Option<Switch>>,
}

const DARWIN: [Triple; 2] = [AARCH64_APPLE_DARWIN, X86_64_APPLE_DARWIN];

impl World {
    fn new(label: &str) -> Self {
        Self::new_at(scratch(label))
    }

    fn new_at(tmp: aterm_tempfile::TempDir) -> Self {
        let fx = Fx::at(tmp);
        for (p, b) in [("ay", 10), ("clean", 20), ("trust", 30), ("trust-ir", 40)] {
            let _ = fx.dev_repo(p, b);
        }
        fx.publish_pkg("ay", 10, &manifest("ay", 10, &[DARWIN[0], DARWIN[1], X86]));
        fx.publish_pkg("clean", 20, &manifest("clean", 20, &DARWIN));
        fx.publish_pkg("trust", 30, &manifest("trust", 30, &DARWIN));
        fx.publish_pkg("trust-ir", 40, &manifest("trust-ir", 40, &DARWIN));
        fx.publish_head(
            7,
            &[("ay", 10), ("clean", 20), ("trust", 30), ("trust-ir", 40)],
        );
        Self::of(fx)
    }

    fn of(fx: Fx) -> Self {
        Self {
            fx,
            host: X86,
            builder: FakeBuilder::can(&[ARM]),
            switch: Mutex::new(None),
        }
    }

    fn after_staging(label: &str, unproven: bool) -> Self {
        static MADE: Made<bool, Held> = Mutex::new(BTreeMap::new());
        let mut tmp = Some(scratch(label));
        let dir = tmp.as_ref().expect("a scratch dir").path().to_path_buf();
        let mut made = None;
        let held = made_once(&MADE, &unproven, &dir, |_| {
            let w = Self::new_at(tmp.take().expect("a scratch dir"));
            w.keyless();
            w.fx.packer.unproven.store(unproven, Ordering::SeqCst);
            ok(w.run(false));
            let held = w.fx.held();
            made = Some(w);
            held
        });
        made.unwrap_or_else(|| Self::of(Fx::laid(tmp.take().expect("a scratch dir"), &held)))
    }

    fn keyless(&self) {
        let _ = std::fs::remove_file(&self.fx.ctx.identity.key);
    }

    fn rekey(&self) {
        std::fs::write(&self.fx.ctx.identity.key, &self.fx.keys.machine_key).expect("key");
    }

    fn run(&self, dry_run: bool) -> (Result<Outcome>, Transcript) {
        let tx = Transcript::memory(linux::ME);
        let opts = Options::new(&self.fx.ctx, self.host, dry_run, &[]).expect("options");
        let sw = |_: &Ctx| -> Result<Switch> {
            Ok(self
                .switch
                .lock()
                .expect("switch")
                .clone()
                .unwrap_or(Switch::Unknown("no trust checkout to read".into())))
        };
        let deps = Deps {
            packer: &self.fx.packer,
            indexer: &self.fx.indexer,
            builder: &self.builder,
            switch: &sw,
        };
        (linux::run(&self.fx.ctx, &opts, &deps, &tx), tx)
    }

    fn staged(&self) -> std::path::PathBuf {
        self.fx.ctx.state_dir.join("staged-index-7")
    }

    fn ready(&self) -> String {
        std::fs::read_to_string(self.staged().join("READY.md")).unwrap_or_default()
    }

    fn manifest_of(&self, p: &str, b: u64) -> String {
        std::fs::read_to_string(linux::staged_manifest(&self.staged(), p, b)).unwrap_or_default()
    }
}

fn ok(r: (Result<Outcome>, Transcript)) -> (Outcome, Transcript) {
    match r.0 {
        Ok(o) => (o, r.1),
        Err(e) => panic!("the run refused: {e}\n{}", r.1.text()),
    }
}

#[test]
fn missing_declared_gnu_git_rows_are_acquired_without_a_repo_build_and_resume_to_publication() {
    let fx = Fx::at(scratch("linux-upstream-git"));
    fx.publish_pkg("git", 12, &manifest("git", 12, &DARWIN));
    fx.publish_head(7, &[("git", 12)]);
    let w = World::of(fx);
    w.fx.packer.upstream.store(true, Ordering::SeqCst);
    w.builder.0.lock().unwrap().clear();
    w.keyless();
    let (dry, _) = ok(w.run(true));
    assert!(matches!(decision(&dry, "git"), Decision::Build { .. }));
    assert!(w.fx.packer.legs().is_empty());
    let (staged, _) = ok(w.run(false));
    assert_eq!(staged.staged, ["git"]);
    assert_eq!(staged.ready, ["git"]);
    assert!(staged.signed.is_empty());
    assert_eq!(w.fx.packer.legs().len(), 2);
    assert!(
        w.fx.packer
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.checkout.as_os_str().is_empty())
    );
    assert!(
        w.fx.calls("clone ").is_empty(),
        "an upstream Git package is not built from the aterm repo"
    );
    w.rekey();
    let (published, _) = ok(w.run(false));
    assert_eq!(published.signed, ["git"]);
    assert_eq!(
        w.fx.packer.legs().len(),
        2,
        "staged qualified rows are reused"
    );
    let (complete, _) = ok(w.run(true));
    assert_eq!(*decision(&complete, "git"), Decision::Ok);
    let published =
        w.fx.public_asset("git", 12, "pkg-git-12.toml")
            .expect("public signed package manifest");
    let package: atpkg::PkgManifest =
        aterm_toml::from_str(std::str::from_utf8(&published).unwrap()).unwrap();
    assert!(package.artifact_for(X86.as_str()).is_some());
    assert!(package.artifact_for(ARM.as_str()).is_some());
    assert!(
        package
            .artifact_for(AARCH64_APPLE_DARWIN.as_str())
            .is_some()
    );
}

#[test]
fn c_source_targets_remain_bound_to_their_own_channel_pin_without_rust_cross_support() {
    let fx = Fx::at(scratch("linux-upstream-target-pin"));
    fx.publish_pkg("git", 12, &manifest("git", 12, &DARWIN));
    let mut text = fx.index_text(7, &[("git", 12)]);
    text.push_str("pin_by_target = { \"aarch64-unknown-linux-gnu\" = { git = 13 } }\n");
    fx.publish_index_text(7, &text);
    let w = World::of(fx);
    w.fx.packer.upstream.store(true, Ordering::SeqCst);
    w.builder.0.lock().unwrap().clear();
    let (outcome, _) = ok(w.run(true));
    let git = outcome
        .table
        .iter()
        .find(|(program, _)| program.name == "git")
        .unwrap();
    assert_eq!(git.0.rows[&ARM], RowState::Skipped);
    assert_eq!(git.0.need(), [X86]);
    assert_eq!(outcome.can, [X86]);
    assert!(w.fx.packer.legs().is_empty());
}

fn decision<'a>(o: &'a Outcome, p: &str) -> &'a Decision {
    &o.table
        .iter()
        .find(|(prog, _)| prog.name == p)
        .unwrap_or_else(|| panic!("no {p} row"))
        .1
}

fn table_row(tx: &Transcript, p: &str) -> String {
    tx.lines()
        .into_iter()
        .find(|l| l.starts_with(&format!("{p} ")))
        .unwrap_or_else(|| panic!("no {p} row in\n{}", tx.text()))
}

#[test]
fn a_latch_outranks_every_quieter_reading_of_a_row() {
    assert_eq!(row_state(true, true, true, true), RowState::Published);
    assert_eq!(row_state(false, true, true, true), RowState::Staged);
    assert_eq!(row_state(false, false, true, true), RowState::Blocked);
    assert_eq!(row_state(false, false, true, false), RowState::Blocked);
    assert_eq!(row_state(false, false, false, true), RowState::Absent);
    assert_eq!(row_state(false, false, false, false), RowState::Skipped);
}

#[test]
fn a_dry_run_sees_every_served_target_and_plans_the_missing_rows() {
    let w = World::new("linux-dry");
    let (o, tx) = ok(w.run(true));
    assert_eq!(*decision(&o, "ay"), Decision::Build { held: vec![] });
    let ay = table_row(&tx, "ay");
    assert!(
        ay.contains("x86_64=published") && ay.contains("aarch64=absent"),
        "{ay}"
    );
    assert!(
        ay.contains("pack aarch64-unknown-linux-gnu") && !ay.contains("x86_64-unknown-linux-gnu"),
        "{ay}"
    );
    let clean = table_row(&tx, "clean");
    assert!(
        clean.contains(
            "BUILD: resolve pinned commit, pack x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu"
        ),
        "{clean}"
    );
    assert_eq!(*decision(&o, "trust"), Decision::ManualBundle);
    assert!(table_row(&tx, "trust-ir").contains("BUILD+HOLD(group rustc: x86_64,aarch64)"));
    assert!(w.fx.packer.legs().is_empty() && w.fx.calls("clone ").is_empty());
    assert!(
        !LaneState::of(LaneKind::Linux, &w.fx.ctx.state_dir)
            .index_seen
            .exists()
    );
}

#[test]
fn a_latch_on_one_target_is_blocked_and_never_stops_the_other() {
    let w = World::new("linux-latch");
    std::fs::create_dir_all(w.staged()).expect("mkdir");
    std::fs::write(
        w.staged()
            .join("BLOCKED-ay-10-aarch64-unknown-linux-gnu.md"),
        "",
    )
    .expect("latch");
    std::fs::write(
        w.staged()
            .join("BLOCKED-clean-20-aarch64-unknown-linux-gnu.md"),
        "",
    )
    .expect("latch");
    let (o, tx) = ok(w.run(true));
    assert_eq!(*decision(&o, "ay"), Decision::Blocked);
    assert!(table_row(&tx, "ay").contains("BLOCKED: latched for aarch64-unknown-linux-gnu"));
    let clean = table_row(&tx, "clean");
    assert!(
        clean.contains("BUILD: resolve pinned commit, pack x86_64-unknown-linux-gnu, merge"),
        "{clean}"
    );
}

#[test]
fn an_x86_only_builder_plans_x86_and_names_the_target_it_leaves_unserved() {
    let mut w = World::new("linux-x86-only");
    w.builder = FakeBuilder::can(&[]);
    let (o, tx) = ok(w.run(true));
    let clean = table_row(&tx, "clean");
    assert!(
        clean.contains("pack x86_64-unknown-linux-gnu, merge") && !clean.contains("BLOCKED"),
        "{clean}"
    );
    assert_eq!(*decision(&o, "ay"), Decision::Partial);
    assert!(table_row(&tx, "ay").contains("not served by this builder: aarch64-unknown-linux-gnu"));
}

#[test]
fn an_aarch64_builder_plans_its_own_target_and_crosses_given_a_toolchain() {
    let mut w = World::new("linux-arm-host");
    w.host = ARM;
    w.builder = FakeBuilder::can(&[]);
    let (_, tx) = ok(w.run(true));
    let clean = table_row(&tx, "clean");
    assert!(
        clean.contains("pack aarch64-unknown-linux-gnu, merge"),
        "{clean}"
    );
    w.builder = FakeBuilder::can(&[X86]);
    let (_, tx) = ok(w.run(true));
    assert!(
        table_row(&tx, "clean")
            .contains("pack x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu")
    );
}

#[test]
fn a_channel_that_splits_the_targets_narrows_to_the_host_and_never_dies() {
    let w = World::new("linux-narrow");
    let mut text = w.fx.index_text(
        8,
        &[("ay", 10), ("clean", 20), ("trust", 30), ("trust-ir", 40)],
    );
    text.push_str("pin_by_target = { \"aarch64-unknown-linux-gnu\" = { ay = 11 } }\n");
    w.fx.publish_index_text(8, &text);
    let (o, tx) = ok(w.run(true));
    assert_eq!(o.can, [X86]);
    assert!(table_row(&tx, "clean").contains("pack x86_64-unknown-linux-gnu, merge"));
}

#[test]
fn staging_extends_the_published_bytes_with_every_target_in_one_manifest() {
    let w = World::new("linux-stage");
    w.keyless();
    let (o, _) = ok(w.run(false));
    let clean = w.manifest_of("clean", 20);
    for t in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
    ] {
        assert!(clean.contains(&format!("target = \"{t}\"")), "{clean}");
    }
    let legs = w.fx.packer.legs();
    let clean_legs: Vec<_> = legs.iter().filter(|l| l.0 == "clean").collect();
    assert_eq!(clean_legs.len(), 2, "{legs:?}");
    assert!(
        clean_legs.iter().all(|l| l.2.is_some()),
        "every leg merges onto a base"
    );
    for t in [X86, ARM] {
        assert!(
            w.staged()
                .join(format!("clean/clean-20-{t}.tar.zst"))
                .is_file()
        );
    }
    let published = String::from_utf8(
        w.fx.staging_asset("clean", 20, "pkg-clean-20.toml")
            .expect("pub"),
    )
    .expect("utf8");
    assert!(
        clean.starts_with(&published),
        "a byte-extension of the published manifest"
    );
    let ay = w.manifest_of("ay", 10);
    assert_eq!(
        ay.matches("target = \"x86_64-unknown-linux-gnu\"").count(),
        1
    );
    assert_eq!(
        ay.matches("target = \"aarch64-unknown-linux-gnu\"").count(),
        1
    );
    assert!(
        !w.staged().join(format!("ay/ay-10-{X86}.tar.zst")).exists(),
        "a published row is never re-packed"
    );
    let ready = w.ready();
    for t in [
        "clean/clean-20-x86_64-unknown-linux-gnu.tar.zst",
        "clean/clean-20-aarch64-unknown-linux-gnu.tar.zst",
        "ay/ay-10-aarch64-unknown-linux-gnu.tar.zst",
    ] {
        assert!(ready.contains(t), "{ready}");
    }
    assert_eq!(o.ready, ["ay", "clean"]);
    assert_eq!(o.held, ["trust-ir"]);
    assert!(o.published.is_none() && w.fx.indexer.count() == 0);
}

#[test]
fn an_x86_only_builder_stages_x86_and_tells_the_signer_what_went_unserved() {
    let mut w = World::new("linux-stage-x86");
    w.builder = FakeBuilder::can(&[]);
    w.keyless();
    ok(w.run(false));
    assert!(
        w.staged()
            .join(format!("clean/clean-20-{X86}.tar.zst"))
            .is_file()
    );
    assert!(
        !w.staged()
            .join(format!("clean/clean-20-{ARM}.tar.zst"))
            .exists()
    );
    assert!(
        !w.staged().join("ay").exists(),
        "ay had nothing this builder could pack"
    );
}

#[test]
fn one_targets_failure_latches_that_target_and_the_other_still_stages() {
    let w = World::new("linux-fail-one");
    w.keyless();
    w.fx.packer.fail_on("clean", ARM);
    let (r, _) = w.run(false);
    let e = r.expect_err("a pack failure ends the run loudly");
    assert!(
        e.to_string()
            .contains("run finished with BLOCKED/ERROR rows"),
        "{e}"
    );
    assert!(
        w.staged()
            .join(format!("BLOCKED-clean-20-{ARM}.md"))
            .is_file()
    );
    assert!(
        !w.staged().join("BLOCKED-clean-20.md").exists(),
        "the latch is per target"
    );
    assert!(
        w.staged()
            .join(format!("clean/clean-20-{X86}.tar.zst"))
            .is_file()
    );
    assert!(
        w.manifest_of("clean", 20)
            .contains("x86_64-unknown-linux-gnu")
    );
}

#[test]
fn a_keyed_box_signs_uploads_to_both_owners_and_publishes_the_follow_up_index() {
    let w = World::new("linux-keyed");
    let (o, _) = ok(w.run(false));
    assert_eq!(o.signed, ["ay", "clean"]);
    assert_eq!(o.published, Some(8));
    let deltas = w.fx.indexer.deltas();
    assert_eq!(deltas.len(), 1);
    assert!(deltas[0].is_empty(), "the follow-up index moves no pin");
    let staged = w.manifest_of("clean", 20);
    for owner in [
        w.fx.staging_asset("clean", 20, "pkg-clean-20.toml"),
        w.fx.public_asset("clean", 20, "pkg-clean-20.toml"),
    ] {
        assert_eq!(
            owner.as_deref(),
            Some(staged.as_bytes()),
            "both owners carry the same bytes"
        );
    }
    assert!(
        w.fx.public_asset("clean", 20, "pkg-clean-20.toml.sig")
            .is_some()
    );
    assert!(
        w.fx.public_asset("clean", 20, &format!("clean-20-{ARM}.tar.zst"))
            .is_some()
    );
    let reservation = w.fx.staging_asset("clean", 20, VERSION_RESERVATION);
    assert!(
        reservation.is_some(),
        "the signed target extension reserves its unchanged version"
    );
    assert_eq!(
        w.fx.public_asset("clean", 20, VERSION_RESERVATION),
        reservation
    );
    let ready = w.ready();
    assert!(
        ready.contains(
            "Signed and published by this run with the machine key at ~/.aterm/release-ring/machine.key:"
        ),
        "{ready}"
    );
}

#[test]
fn a_keyed_run_that_stops_before_the_index_leaves_the_ceremony() {
    let w = World::new("linux-index-fails");
    w.fx.indexer.fail.store(true, Ordering::SeqCst);
    let (r, _) = w.run(false);
    let e = r.expect_err("a failed index ends the run loudly");
    assert!(
        e.to_string()
            .contains("the follow-up index 8 did not publish"),
        "{e}"
    );
    let ready = w.ready();
    assert!(
        ready.contains("If its log has no \"published atpkg-index-8\", run the ceremony below."),
        "{ready}"
    );
    w.fx.indexer.fail.store(false, Ordering::SeqCst);
    let (o, _) = ok(w.run(false));
    assert!(o.ready.is_empty() && o.signed.is_empty());
    assert_eq!(o.published, Some(8));
}

#[test]
fn a_keyed_box_with_no_channel_token_signs_nothing() {
    let w = World::new("linux-no-token");
    std::fs::remove_file(&w.fx.ctx.tokens.channel).expect("rm token");
    let (r, _) = w.run(false);
    let e = r.expect_err("no token refuses");
    assert!(
        e.to_string()
            .contains("channel token file missing/empty: ~/.secrets/gh_access_token_alabsystems"),
        "{e}"
    );
    assert!(
        !linux::staged_manifest(&w.staged(), "clean", 20)
            .with_extension("toml.sig")
            .exists()
    );
}

#[test]
fn owed_uploads_with_no_channel_token_say_why_in_ready_md() {
    // The default `Gh` host (§1 rule 16's third exception): READY.md's keyed, nothing-ready
    // arm names the refusal that stopped the follow-up index.
    let w = World::new("linux-no-token-owed");
    w.fx.indexer.fail.store(true, Ordering::SeqCst);
    let _ = w.run(false);
    w.fx.indexer.fail.store(false, Ordering::SeqCst);
    std::fs::remove_file(&w.fx.ctx.tokens.channel).expect("rm token");
    let (r, tx) = w.run(false);
    let e = r.expect_err("no channel token for the owed uploads");
    assert_eq!(e.exit_code(), 1, "{e}\n{}", tx.text());
    let ready = w.ready();
    assert!(
        ready.contains("Signed and uploaded by an earlier run: ay clean.")
            && ready.contains("READY (sign + publish now): <none>"),
        "{ready}"
    );
    assert!(
        ready.contains(
            "the channel token at ~/.secrets/gh_access_token_alabsystems is missing or empty"
        ),
        "{ready}"
    );
}

#[test]
fn a_staged_merge_that_no_longer_extends_the_published_bytes_takes_the_ceremony_with_it() {
    let w = World::after_staging("linux-prefix", false);
    let m = linux::staged_manifest(&w.staged(), "clean", 20);
    let text = std::fs::read_to_string(&m).expect("staged");
    std::fs::write(
        &m,
        format!("# restaged on a base another machine has since extended\n{text}"),
    )
    .expect("write");
    w.rekey();
    let (r, _) = w.run(false);
    let e = r.expect_err("the byte-prefix gate refuses");
    assert!(
        e.to_string().contains(
            "clean@20: the published manifest on alabsystems/clean is no longer a \
             byte-prefix of the staged merge"
        ),
        "{e}"
    );
    assert!(
        !w.staged().join("READY.md").exists(),
        "a ceremony beside the refusal would clobber the row it protects"
    );
    assert!(!m.with_extension("toml.sig").exists());
}

fn unproven(label: &str, switch: Option<Switch>) -> (World, Result<Outcome>) {
    let w = World::after_staging(label, true);
    w.rekey();
    *w.switch.lock().expect("switch") = switch;
    let (r, _) = w.run(false);
    (w, r)
}

#[test]
fn an_unproven_row_is_signed_only_while_the_switch_still_reads_dormant() {
    let (w, r) = unproven("linux-unproven-unknown", None);
    let e = r.expect_err("nothing shows the switch dormant");
    assert!(
        e.to_string().contains(
            "the staged manifest's new rows were packed by a compiler with no lineage receipt"
        ),
        "{e}"
    );
    assert!(
        e.to_string()
            .contains("nothing shows it still is (no trust checkout to read)"),
        "{e}"
    );
    assert!(!w.staged().join("READY.md").exists());
    let (w, r) = unproven("linux-unproven-live", Some(Switch::Live));
    let e = r.expect_err("a minted re-pin refuses");
    assert!(
        e.to_string()
            .contains("origin main now carries a minted re-pin — nothing signed; delete"),
        "{e}"
    );
    assert!(!w.staged().join("READY.md").exists());
    let (w, r) = unproven("linux-unproven-dormant", Some(Switch::Dormant));
    let o = r.expect("a dormant switch signs as staged");
    assert_eq!(o.signed, ["ay", "clean"]);
    assert!(
        linux::staged_manifest(&w.staged(), "clean", 20)
            .with_extension("toml.sig")
            .exists()
    );
}

#[test]
fn an_unproven_row_in_the_published_base_is_never_judged_again() {
    let w = World::new("linux-unproven-base");
    let mut base = manifest("clean", 20, &[]);
    base.push_str(&row("clean", 20, AARCH64_APPLE_DARWIN, true));
    base.push_str(&row("clean", 20, X86_64_APPLE_DARWIN, false));
    w.fx.publish_pkg("clean", 20, &base);
    let (o, _) = ok(w.run(false));
    assert!(
        o.signed.contains(&"clean".to_string()),
        "only this lane's rows are judged"
    );
}

#[test]
fn an_unfetchable_manifest_is_an_error_row_that_says_why() {
    let w = World::new("linux-unfetchable");
    w.fx.host.put(
        &w.fx.ctx.staging_slug("clean"),
        "atpkg-clean-20",
        atpkg_publish::host::fake::FakeRelease::default(),
    );
    let (o, tx) = ok(w.run(true));
    let Decision::Error(why) = decision(&o, "clean") else {
        panic!("{:?}", decision(&o, "clean"))
    };
    assert!(
        why.starts_with(
            "cannot fetch pkg-clean-20.toml from release atpkg-clean-20 on alabsystems/clean:"
        ),
        "{why}"
    );
    assert!(table_row(&tx, "clean").contains("unfetchable"));
    let (r, _) = w.run(false);
    assert!(r.is_err(), "a real run with an ERROR row ends loudly");
}

#[test]
fn an_index_or_pin_downgrade_refuses_the_linux_run() {
    let w = World::after_staging("linux-downgrade", false);
    let state = LaneState::of(LaneKind::Linux, &w.fx.ctx.state_dir);
    std::fs::write(&state.index_seen, "9\n").expect("floor");
    let (r, _) = w.run(true);
    assert!(
        r.expect_err("index downgrade")
            .to_string()
            .contains("channel head is index 7 but this lane already processed 9")
    );
    std::fs::write(&state.index_seen, "7\n").expect("floor");
    w.fx.publish_head(
        8,
        &[("ay", 10), ("clean", 19), ("trust", 30), ("trust-ir", 40)],
    );
    let (r, _) = w.run(true);
    assert!(
        r.expect_err("pin downgrade")
            .to_string()
            .contains("PIN DOWNGRADE on the channel: clean 20 -> 19 in index-8")
    );
}

#[test]
fn the_lane_serves_linux_targets_on_a_linux_host_only() {
    let fx = Fx::new("linux-host");
    let e = Options::new(&fx.ctx, AARCH64_APPLE_DARWIN, true, &[]).expect_err("a Mac");
    assert!(e.to_string().contains("this lane packs Linux rows"), "{e}");
    let e = Options::new(&fx.ctx, X86, true, &[X86_64_APPLE_DARWIN]).expect_err("a darwin target");
    assert_eq!(e.exit_code(), 2);
    let all = Options::new(&fx.ctx, X86, true, &[]).expect("default");
    assert_eq!(
        all.triples,
        [X86, ARM],
        "every Linux target the client ships, derived"
    );
}

fn publish_public_only(w: &World, p: &str, b: u64, text: &str) {
    let slug = w.fx.ctx.public_slug(p);
    let tag = format!("atpkg-{p}-{b}");
    let name = format!("pkg-{p}-{b}.toml");
    w.fx.host
        .put_asset(&slug, &tag, &name, text.as_bytes().to_vec());
    w.fx.host.put_asset(
        &slug,
        &tag,
        &format!("{name}.sig"),
        w.fx.keys.sign(text.as_bytes()),
    );
}

fn uploads_of(w: &World, tag: &str) -> Vec<String> {
    w.fx.calls("upload ")
        .into_iter()
        .filter(|c| c.contains(&format!(" {tag} ")))
        .collect()
}

fn unverifiable(label: &str, tamper: bool) -> (World, Result<Outcome>, Vec<u8>) {
    let w = World::after_staging(label, false);
    let mut theirs = manifest("clean", 20, &DARWIN);
    theirs.push_str(&row("clean", 20, X86, false));
    publish_public_only(&w, "clean", 20, &theirs);
    let staging = w.fx.ctx.staging_slug("clean");
    if tamper {
        w.fx.host.put_asset(
            &staging,
            "atpkg-clean-20",
            "pkg-clean-20.toml.sig",
            vec![7u8; 64],
        );
    } else {
        w.fx.host.put(
            &staging,
            "atpkg-clean-20",
            atpkg_publish::host::fake::FakeRelease::default(),
        );
    }
    w.rekey();
    let (r, _) = w.run(false);
    (w, r, theirs.into_bytes())
}

#[test]
fn a_manifest_that_cannot_be_verified_is_never_signed_over_on_either_owner() {
    for (label, tamper) in [
        ("linux-unverifiable-absent", false),
        ("linux-unverifiable-sig", true),
    ] {
        let (w, r, theirs) = unverifiable(label, tamper);
        let e = r.expect_err("an ERROR row ends the run loudly");
        assert!(
            e.to_string()
                .contains("run finished with BLOCKED/ERROR rows"),
            "{label}: {e}"
        );
        assert!(
            uploads_of(&w, "atpkg-clean-20").is_empty(),
            "{label}: nothing uploaded for clean: {:?}",
            w.fx.calls("upload ")
        );
        assert_eq!(
            w.fx.public_asset("clean", 20, "pkg-clean-20.toml")
                .as_deref(),
            Some(theirs.as_slice()),
            "{label}: the other machine's row survives"
        );
        assert!(
            !linux::staged_manifest(&w.staged(), "clean", 20)
                .with_extension("toml.sig")
                .exists(),
            "{label}: the key never touched clean"
        );
    }
}

#[test]
fn a_public_manifest_another_machine_extended_refuses_the_signing() {
    let w = World::after_staging("linux-public-extended", false);
    let mut theirs = manifest("clean", 20, &DARWIN);
    theirs.push_str(&row("clean", 20, ARM, false));
    publish_public_only(&w, "clean", 20, &theirs);
    w.rekey();
    let (r, _) = w.run(false);
    let e = r.expect_err("the public owner's bytes are not a prefix of the merge");
    assert!(
        e.to_string().contains(
            "clean@20: the published manifest on alabsystems/clean is no longer a byte-prefix \
             of the staged merge"
        ),
        "{e}"
    );
    assert!(
        w.fx.calls("upload ").is_empty(),
        "nothing uploaded anywhere"
    );
    assert_eq!(
        w.fx.public_asset("clean", 20, "pkg-clean-20.toml")
            .as_deref(),
        Some(theirs.as_bytes())
    );
    assert_eq!(w.fx.indexer.count(), 0);
}

#[test]
fn a_key_the_roster_does_not_list_signs_and_uploads_nothing() {
    let w = World::new("linux-unrostered");
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
    let e = e.to_string();
    assert!(e.contains("machine 'm99' is NOT on the roster"), "{e}");
    assert!(e.contains("— nothing signed"), "{e}");
    assert!(
        w.fx.calls("upload ").is_empty(),
        "nothing uploaded on either owner"
    );
    assert_eq!(w.fx.indexer.count(), 0);
    for p in ["ay", "clean"] {
        let staged = linux::staged_manifest(&w.staged(), p, if p == "ay" { 10 } else { 20 });
        assert!(staged.is_file(), "{p} stays staged for a rostered machine");
        assert!(
            !staged.with_extension("toml.sig").exists(),
            "{p} carries no signature"
        );
    }
}

#[test]
fn a_public_upload_an_earlier_run_owed_is_finished_before_the_index() {
    let w = World::new("linux-public-owed");
    w.fx.indexer.fail.store(true, Ordering::SeqCst);
    let _ = w.run(false);
    w.fx.indexer.fail.store(false, Ordering::SeqCst);
    let original = manifest("clean", 20, &DARWIN);
    let public = w.fx.ctx.public_slug("clean");
    let mut assets = std::collections::BTreeMap::new();
    assets.insert(
        "pkg-clean-20.toml".to_string(),
        original.clone().into_bytes(),
    );
    assets.insert(
        "pkg-clean-20.toml.sig".to_string(),
        w.fx.keys.sign(original.as_bytes()),
    );
    w.fx.host.put(
        &public,
        "atpkg-clean-20",
        atpkg_publish::host::fake::FakeRelease {
            draft: false,
            prerelease: true,
            assets,
        },
    );
    let (o, _) = ok(w.run(false));
    let staged = w.manifest_of("clean", 20);
    assert_eq!(
        w.fx.public_asset("clean", 20, "pkg-clean-20.toml")
            .as_deref(),
        Some(staged.as_bytes())
    );
    for t in [X86, ARM] {
        assert!(
            w.fx.public_asset("clean", 20, &format!("clean-20-{t}.tar.zst"))
                .is_some(),
            "the {t} tarball the public owner lacked"
        );
    }
    assert_eq!(o.published, Some(8));
}

#[test]
fn a_public_copy_that_is_not_the_signed_bytes_nor_their_prefix_publishes_no_index() {
    let w = World::new("linux-public-diverged");
    w.fx.indexer.fail.store(true, Ordering::SeqCst);
    let _ = w.run(false);
    w.fx.indexer.fail.store(false, Ordering::SeqCst);
    let mut theirs = manifest("clean", 20, &DARWIN);
    theirs.push_str("\n# another machine's edit\n");
    publish_public_only(&w, "clean", 20, &theirs);
    let calls = w.fx.indexer.count();
    let (r, _) = w.run(false);
    assert!(r.expect_err("diverged").to_string().contains(
        "serves a manifest that is neither the one an earlier run signed nor a byte-prefix of it"
    ),);
    assert_eq!(
        w.fx.indexer.count(),
        calls,
        "no index over a public copy it cannot vouch for"
    );
    assert_eq!(
        w.fx.public_asset("clean", 20, "pkg-clean-20.toml")
            .as_deref(),
        Some(theirs.as_bytes())
    );
}

#[test]
fn a_real_run_needs_the_staging_token_and_a_dry_run_does_not() {
    let w = World::new("linux-no-staging-token");
    std::fs::remove_file(&w.fx.ctx.tokens.staging).expect("rm token");
    let (r, _) = w.run(false);
    let e = r.expect_err("no staging identity");
    assert_eq!(e.exit_code(), 3, "{e}");
    assert!(
        e.to_string()
            .contains("staging token file missing/empty: ~/.secrets/gh_access_token"),
        "{e}"
    );
    assert!(w.fx.calls("clone ").is_empty(), "nothing cloned or latched");
    assert!(!w.staged().exists());
    ok(w.run(true));
}

#[test]
fn a_scratch_clone_is_fetched_as_the_staging_identity_never_by_its_origin() {
    let w = World::after_staging("linux-fetch", false);
    let clone = w.fx.ctx.state_dir.join("progs/clean");
    super::fixture::git(
        &clone,
        &["remote", "set-url", "origin", "/nonexistent/clean"],
    );
    let _ = super::fixture::commit(&w.fx.path("dev/clean"), "c21", None);
    w.fx.publish_pkg("clean", 21, &manifest("clean", 21, &DARWIN));
    w.fx.publish_head(
        8,
        &[("ay", 10), ("clean", 21), ("trust", 30), ("trust-ir", 40)],
    );
    let (o, tx) = ok(w.run(false));
    assert!(o.staged.contains(&"clean".to_string()), "{}", tx.text());
    assert!(
        w.fx.calls("fetch-repo ")
            .contains(&"fetch-repo alabsystems/clean".to_string()),
        "{:?}",
        w.fx.host.calls()
    );
    assert!(
        !w.fx
            .ctx
            .state_dir
            .join("staged-index-8/BLOCKED-clean-21.md")
            .exists()
    );
}

#[test]
fn the_lane_plans_the_build_its_targets_resolve_not_the_bare_pin() {
    let w = World::new("linux-target-pins");
    w.fx.publish_pkg("ay", 11, &manifest("ay", 11, &DARWIN));
    let mut text = w.fx.index_text(
        8,
        &[("ay", 10), ("clean", 20), ("trust", 30), ("trust-ir", 40)],
    );
    text.push_str(
        "pin_by_target = { \"x86_64-unknown-linux-gnu\" = { ay = 11 }, \
         \"aarch64-unknown-linux-gnu\" = { ay = 11 } }\n",
    );
    w.fx.publish_index_text(8, &text);
    let (o, tx) = ok(w.run(true));
    let ay = o
        .table
        .iter()
        .find(|(p, _)| p.name == "ay")
        .map(|(p, _)| p.build);
    assert_eq!(
        ay,
        Some(11),
        "every Linux client resolves ay 11, never the bare pin 10"
    );
    assert_eq!(o.can, [X86, ARM], "no divergence, nothing narrowed");
    assert!(
        table_row(&tx, "ay").contains("pack x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu"),
        "build 11 carries no Linux row yet: {}",
        tx.text()
    );
}

#[test]
fn a_signed_manifest_the_client_would_refuse_is_never_uploaded() {
    let w = World::after_staging("linux-client-refuses", false);
    let m = linux::staged_manifest(&w.staged(), "clean", 20);
    let mut text = std::fs::read_to_string(&m).expect("staged");
    text.push_str(
        "\n[[artifact]]\ntarget = \"riscv64gc-unknown-linux-gnu\"\nkind = \"vendor-fetch\"\n",
    );
    std::fs::write(&m, text).expect("write");
    w.rekey();
    let (r, _) = w.run(false);
    let e = r.expect_err("the pair does not verify");
    assert!(
        e.to_string()
            .contains("clean@20: the signed manifest does not verify through the client's chain"),
        "{e}"
    );
    assert!(
        w.fx.calls("upload ").is_empty(),
        "nothing uploaded on either owner"
    );
    assert!(
        !m.with_extension("toml.sig").exists(),
        "the refused pair loses its signature"
    );
    assert!(!w.staged().join("READY.md").exists());
    assert_eq!(w.fx.indexer.count(), 0);
}

#[test]
fn a_pinned_harness_is_apple_only_and_the_linux_lane_builds_and_fetches_nothing_for_it() {
    let w = World::new("linux-harness");
    let _ = w.fx.dev_repo("muse-alab", 3);
    w.fx.publish_pkg(
        "muse-alab",
        3,
        &manifest("muse-alab", 3, &[AARCH64_APPLE_DARWIN]),
    );
    w.fx.publish_head(
        8,
        &[
            ("ay", 10),
            ("clean", 20),
            ("trust", 30),
            ("trust-ir", 40),
            ("muse-alab", 3),
        ],
    );
    let (o, tx) = ok(w.run(true));
    assert_eq!(*decision(&o, "muse-alab"), Decision::AppleOnly);
    assert!(
        table_row(&tx, "muse-alab").contains("APPLE-ONLY: a harness ships Apple silicon only"),
        "{}",
        tx.text()
    );
    assert_eq!(
        *decision(&o, "clean"),
        Decision::Build { held: vec![] },
        "the other rows are planned as before"
    );
    w.keyless();
    let (o, tx) = ok(w.run(false));
    assert!(
        !o.staged.contains(&"muse-alab".to_string()),
        "{}",
        tx.text()
    );
    assert!(
        w.fx.packer.legs().iter().all(|(p, _, _)| p != "muse-alab"),
        "no Linux row is packed: {:?}",
        w.fx.packer.legs()
    );
    let touched: Vec<String> =
        w.fx.host
            .calls()
            .into_iter()
            .filter(|c| c.contains("muse-alab"))
            .collect();
    assert!(
        touched.is_empty(),
        "its staging manifest is never fetched nor its repo cloned: {touched:?}"
    );
}

/// WP12's Linux tests (plan §16): the pre-claim checks 6 and 7 and READY.md's spellings under
/// each host choice. Each world is built by `World::new`, never `World::after_staging`, whose
/// staging run was made once under `Gh`.
mod host_choice {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use atpkg_publish::ctx::HostChoice;
    use atpkg_publish::host::fake::{FakeFail, FakeHost, FakeOp};
    use atpkg_publish::host::{HostError, Probe, Release, ReleaseHost, Replace, Slug};

    use super::{World, ok};

    /// A `FakeHost` whose `write_preflight` refuses each slug `refused` names (`owner/repo`),
    /// and delegates every other call.
    struct RefusesOwner {
        inner: Arc<FakeHost>,
        refused: Vec<String>,
    }

    impl ReleaseHost for RefusesOwner {
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
            let named = slug.to_string();
            if self.refused.iter().any(|r| r.eq_ignore_ascii_case(&named)) {
                Err(HostError::Refused("refused by the test".into()))
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
        fn fetch_main(&self, slug: &Slug, dir: &Path, into: &str) -> Result<(), HostError> {
            self.inner.fetch_main(slug, dir, into)
        }
        fn repo_seen(&self, slug: &Slug) -> Result<bool, HostError> {
            self.inner.repo_seen(slug)
        }
        fn published_releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
            self.inner.published_releases(slug)
        }
        fn published_by_tag(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
            self.inner.published_by_tag(slug, tag)
        }
        fn retry_pause(&self) -> std::time::Duration {
            self.inner.retry_pause()
        }
        fn reads_anonymously(&self) -> bool {
            self.inner.reads_anonymously()
        }
    }

    /// A fresh world whose context is built for `choice`, with neither token file.
    fn world(label: &str, choice: HostChoice) -> World {
        let mut w = World::new(label);
        w.fx.ctx = w.fx.ctx.clone().with_host_choice(choice);
        for token in [&w.fx.ctx.tokens.staging, &w.fx.ctx.tokens.channel] {
            std::fs::remove_file(token).expect("rm token");
        }
        w
    }

    #[test]
    fn linux_preflights_ask_the_host_under_agh() {
        // Check 6: the staging writer is asked before any clone or latch.
        let w = world("linux-agh-check6", HostChoice::Agh);
        let staging = w.fx.ctx.staging_slug("aterm");
        w.fx.host.fail_next(FakeOp::Preflight, FakeFail::Refuse);
        let (r, _) = w.run(false);
        let e = r.expect_err("no staging writer");
        assert_eq!(e.exit_code(), 3, "{e}");
        let why = e.to_string();
        assert!(
            why.contains(&format!("staging writer unavailable on {staging}")),
            "{why}"
        );
        assert!(why.contains("refused by the test"), "{why}");
        assert!(w.fx.calls("clone ").is_empty(), "nothing cloned or latched");
        assert!(!w.staged().exists());
        let asked = w.fx.calls("preflight ").len();
        ok(w.run(true));
        assert_eq!(
            w.fx.calls("preflight ").len(),
            asked,
            "a dry run asks no writer"
        );

        // Unarmed: a keyed run signs and publishes with no token file present. Each staging
        // repository the uploads write is asked before the public ones.
        let w = world("linux-agh-keyed", HostChoice::Agh);
        let (o, tx) = ok(w.run(false));
        assert_eq!(o.signed, ["ay", "clean"], "{}", tx.text());
        assert_eq!(o.published, Some(8));
        let public = w.fx.ctx.accounts.public.clone();
        let staging = |repo: &str| w.fx.ctx.staging_slug(repo).to_string();
        assert_eq!(
            w.fx.calls("preflight "),
            [
                format!("preflight {}", staging("aterm")),
                format!("preflight {}", staging("ay")),
                format!("preflight {}", staging("clean")),
                format!("preflight {public}/aterm"),
                format!("preflight {public}/ay"),
                format!("preflight {public}/clean"),
            ]
        );

        // Check 6 per program: one staging repository the uploads write refused, nothing
        // signed, and READY.md says why.
        let mut w = world("linux-agh-check6-programs", HostChoice::Agh);
        let clean = w.fx.ctx.staging_slug("clean").to_string();
        w.fx.ctx.hosts.staging = Arc::new(RefusesOwner {
            inner: w.fx.host.clone(),
            refused: vec![clean.clone()],
        });
        let (r, _) = w.run(false);
        let e = r.expect_err("no staging writer on one program");
        assert_eq!(e.exit_code(), 1, "{e}");
        let why = e.to_string();
        assert!(
            why.contains(&format!("staging writer unavailable on {clean}")),
            "{why}"
        );
        assert!(why.contains("nothing signed"), "{why}");
        let ready = w.ready();
        assert!(
            ready.contains(&format!("the staging writer on {clean} is unavailable")),
            "{ready}"
        );
        assert!(
            !super::linux::staged_manifest(&w.staged(), "clean", 20)
                .with_extension("toml.sig")
                .exists()
        );

        // Check 7: the public writer refused, nothing signed.
        let mut w = world("linux-agh-check7", HostChoice::Agh);
        w.fx.ctx.hosts.channel = Arc::new(RefusesOwner {
            inner: w.fx.host.clone(),
            refused: vec![format!("{public}/aterm")],
        });
        let (r, _) = w.run(false);
        let e = r.expect_err("no public writer");
        assert_eq!(e.exit_code(), 1, "{e}");
        let why = e.to_string();
        assert!(
            why.contains(&format!("public writer unavailable on {public}/aterm")),
            "{why}"
        );
        assert!(why.contains("refused by the test"), "{why}");
        assert!(why.contains("nothing signed"), "{why}");
        let ready = w.ready();
        assert!(
            ready.contains(&format!(
                "the public writer on {public}/aterm is unavailable"
            )),
            "{ready}"
        );
        assert!(
            !super::linux::staged_manifest(&w.staged(), "clean", 20)
                .with_extension("toml.sig")
                .exists()
        );

        // Check 7 with only the uploads an earlier run signed owed: READY.md says why the
        // follow-up index is not published.
        let mut w = world("linux-agh-check7-earlier", HostChoice::Agh);
        w.fx.indexer.fail.store(true, Ordering::SeqCst);
        let _ = w.run(false);
        w.fx.indexer.fail.store(false, Ordering::SeqCst);
        w.fx.ctx.hosts.channel = Arc::new(RefusesOwner {
            inner: w.fx.host.clone(),
            refused: vec![format!("{public}/aterm")],
        });
        let (r, tx) = w.run(false);
        let e = r.expect_err("no public writer for the owed uploads");
        assert_eq!(e.exit_code(), 1, "{e}\n{}", tx.text());
        let ready = w.ready();
        assert!(
            ready.contains("Signed and uploaded by an earlier run: ay clean.")
                && ready.contains("READY (sign + publish now): <none>"),
            "{ready}"
        );
        assert!(
            ready.contains(&format!(
                "the public writer on {public}/aterm is unavailable"
            )),
            "{ready}"
        );
    }

    #[test]
    fn ready_md_spells_each_owner_by_its_host() {
        // READY.md, and the world's owners, read at run time so no owner is written here.
        let ceremony = |choice: HostChoice| {
            let label = format!("linux-ready-{}", choice.as_str());
            // The default reads its token files, as M1 did; the agh choices read none.
            let w = if choice == HostChoice::Gh {
                World::new(&label)
            } else {
                world(&label, choice)
            };
            w.keyless();
            ok(w.run(false));
            (w.ready(), w.fx.ctx.accounts.clone())
        };
        let channel = "~/.secrets/gh_access_token_alabsystems";

        let (gh, _) = ceremony(HostChoice::Gh);
        assert!(
            gh.contains("```sh\natpkg-publish lane run linux --sha256 "),
            "{gh}"
        );
        assert!(
            gh.contains(&format!("and owner tokens including {channel}.")),
            "{gh}"
        );
        assert!(gh.contains("`atpkg-publish index --upload`"), "{gh}");

        let (staging, accounts) = ceremony(HostChoice::AghStaging);
        assert!(
            staging.contains("```sh\natpkg-publish --host agh-staging lane run linux --sha256 "),
            "{staging}"
        );
        assert!(
            staging.contains(&format!(
                "and agh.toml's writer for {} and the channel token {channel}.",
                accounts.staging
            )),
            "{staging}"
        );
        assert!(
            staging.contains("`atpkg-publish index --upload`"),
            "{staging}"
        );

        let (agh, accounts) = ceremony(HostChoice::Agh);
        assert!(
            agh.contains("```sh\natpkg-publish --host agh lane run linux --sha256 "),
            "{agh}"
        );
        let writers = if accounts.staging.eq_ignore_ascii_case(&accounts.public) {
            format!("and agh.toml's writer for {}.", accounts.public)
        } else {
            format!(
                "and agh.toml's writers for {} and {}.",
                accounts.staging, accounts.public
            )
        };
        assert!(agh.contains(&writers), "{agh}");
        assert!(
            agh.contains("`atpkg-publish --host agh index --upload`"),
            "{agh}"
        );
        assert!(!agh.contains("gh_access_token"), "{agh}");
        assert!(!agh.lines().any(|l| l.starts_with("gh ")), "{agh}");
    }
}
