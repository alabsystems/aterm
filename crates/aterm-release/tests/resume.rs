// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resume/recut logic proofs (release spec §5): the journal's re-entry
//! contract and the remote-derived cut-mode decision table, as PURE logic
//! over fake remote state — no network, no git. Plus the pure publish
//! helpers the pipeline hangs off (version bump, monotonic
//! gate, channel-floor carry-forward, exhaustive status selection) and the
//! hand-rolled CLI parse table.

// The release crate is a binary on purpose (the spec's §9 file plan has no
// lib.rs), so the integration tests compile the modules under test directly.
// publish/verify pull in every pipeline stage through `crate::`, hence the
// full mount list.
#[path = "../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
#[path = "../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use publish::{Journal, STEPS};
use verify::{CutMode, RemoteState};

static TMPDIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestTempDir(PathBuf);

impl std::ops::Deref for TestTempDir {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tmpdir(name: &str) -> TestTempDir {
    let sequence = TMPDIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("resume")
        .join(format!("{name}-{}-{sequence}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create tmpdir");
    TestTempDir(dir)
}

fn state(current_version: &str, has_section: bool, published: bool) -> RemoteState {
    RemoteState {
        current_version: current_version.to_string(),
        changelog_has_section: has_section,
        published,
    }
}

// ---------------------------------------------------------------------------
// the §5 remote-derived decision table (journal absent ⇒ derive from remote)
// ---------------------------------------------------------------------------

/// The whole table in one place, spec §5's sentence as executable rows, under
/// the single-version scheme: `RemoteState.current_version` IS the release
/// version, `[workspace.package] version` as written, so a cut NEVER invents a
/// successor. "workspace-derived version already
/// rolled into `## [X.Y.Z]` + no published vX.Y.Z release ⇒ recut" —
/// everything else is a fresh cut of that same version (or a refusal).
#[test]
fn remote_derived_cut_mode_decision_table() {
    let fresh = |v: &str| CutMode::Fresh {
        version: v.to_string(),
    };
    let recut = |v: &str| CutMode::Recut {
        version: v.to_string(),
    };

    // (workspace version, section?, published?) → want
    let table: &[(&str, bool, bool, CutMode)] = &[
        // Steady state: the operator's bump is the ONLY thing that advances the
        // version.
        ("0.3.0", false, false, fresh("0.3.0")),
        // THE wedge signature: roll+claim landed, nothing published ⇒ recut.
        ("0.2.0", true, false, recut("0.2.0")),
        // Negative control for the RETIRED ledger-tail bump: the old default
        // path answered fresh("0.10.0") here. There is no arithmetic left.
        ("0.9.0", false, false, fresh("0.9.0")),
        ("1.99.0", false, false, fresh("1.99.0")),
    ];
    for (short, section, published, want) in table {
        let got = verify::derive_cut_mode(&state(short, *section, *published))
            .unwrap_or_else(|e| panic!("({short}, {section}, {published}) errored: {e}"));
        assert_eq!(&got, want, "({short}, {section}, {published})");
    }
}

/// Cutting twice without bumping `[workspace.package] version` is refused. The
/// message must name the Cargo.toml bump (with the exact next version) and keep
/// the yank escape hatch.
#[test]
fn recutting_a_published_version_is_refused() {
    let err = verify::derive_cut_mode(&state("0.2.0", true, true))
        .unwrap_err()
        .to_string();
    assert!(err.contains("v0.2.0 is already published"), "{err}");
    assert!(
        err.contains("bump [workspace.package] version in Cargo.toml on main"),
        "{err}"
    );
    assert!(err.contains("the next release is v0.3.0"), "{err}");
    assert!(
        err.contains("`pub stage aterm` and `pub publish aterm`, then cut"),
        "a cut builds the published commit, so the next version must be published \
         first: {err}"
    );
    assert!(
        err.contains("targo --unverified ship yank <build>"),
        "{err}"
    );

    // A published version with no rolled section is the same refusal: the
    // guard keys on "published", never on the changelog.
    assert!(verify::derive_cut_mode(&state("0.2.0", false, true)).is_err());
}

// ---------------------------------------------------------------------------
// journal-based re-entry
// ---------------------------------------------------------------------------

fn journal() -> Journal {
    Journal {
        verify_pubkey: None,
        format: publish::JOURNAL_FORMAT,
        version: "0.26.0".into(),
        build_number: 1_783_918_101,
        commit: "aed5a06caed5a06caed5a06caed5a06caed5a06c".into(),
        min_build: None,
        arm64_only: false,
        linux: None,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        draft_create_issued: false,
        upload_intents: Vec::new(),
        mirror_release_id: None,
        mirror_create_issued: false,
        mirror_upload_intents: Vec::new(),
        done: vec![],
    }
}

/// The step list IS the resume contract (spec §7 order) — pin it so a
/// reordering can't silently change what "--resume from selfcheck" means.
#[test]
fn pipeline_step_order_is_the_spec_7_order() {
    assert_eq!(
        STEPS,
        [
            "lock",
            "build",
            "selfcheck",
            "draft",
            "upload",
            "preflip",
            "tag",
            "flip",
            "archive",
            "verify",
            // The public-channel mirror runs AFTER the private release is
            // fully verified and BEFORE the lease is released, so a mirror
            // failure is loud and resumable rather than a silently
            // private-only release the fleet can never see.
            "mirror",
            // `unlock` is LAST (2026-09-23): the website follows the cut AFTER
            // the pipeline, best-effort and unjournaled, so a site failure can
            // never park the journal and block the next cut.
            "unlock",
        ]
    );
    assert!(
        !STEPS.contains(&"site"),
        "the retired `site` step must never be journaled again"
    );
}

#[test]
fn first_incomplete_walks_the_step_order() {
    let mut j = journal();
    assert_eq!(
        j.first_incomplete(),
        Some("lock"),
        "a fresh journal acquires the remote lease before build"
    );
    j.done = vec!["lock".into(), "build".into(), "selfcheck".into()];
    assert_eq!(j.first_incomplete(), Some("draft"));
    j.done = [
        "lock",
        "build",
        "selfcheck",
        "draft",
        "upload",
        "preflip",
        "tag",
        "flip",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(
        j.first_incomplete(),
        Some("archive"),
        "a crash after visibility resumes into channel convergence before verify"
    );
    // Completion ORDER in the file is irrelevant — only membership counts.
    j.done = vec![
        "selfcheck".into(),
        "lock".into(),
        "build".into(),
        "draft".into(),
    ];
    assert_eq!(j.first_incomplete(), Some("upload"));
    // A gap resumes at the GAP, not after the highest completed step: the
    // journal records what finished; skipping an incomplete earlier one is
    // never safe.
    j.done = vec!["lock".into(), "build".into(), "draft".into()];
    assert_eq!(j.first_incomplete(), Some("selfcheck"));
    j.done = STEPS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        j.first_incomplete(),
        None,
        "a completed cut has nothing to resume"
    );
}

#[test]
fn current_journal_done_state_is_an_exact_canonical_prefix() {
    let dir = tmpdir("journal-prefix");
    let path = dir.join("state.toml");
    for done in [
        vec!["build".to_string()],
        vec!["lock".to_string(), "draft".to_string()],
        vec!["lock".to_string(), "lock".to_string()],
        vec!["lock".to_string(), "unknown".to_string()],
    ] {
        let mut corrupted = journal();
        corrupted.done = done;
        assert!(corrupted.save(&path).is_err());
    }
    let mut valid = journal();
    valid.done = STEPS[..5].iter().map(|step| (*step).to_string()).collect();
    valid.release_id = Some(55);
    valid.draft_create_issued = true;
    valid.upload_intents = vec!["aterm-0.26.0.dmg".into()];
    valid.save(&path).unwrap();
    assert_eq!(Journal::load(&path).unwrap().unwrap(), valid);

    // The retired two-component spelling is no longer a version this cutter
    // will journal — a current-format journal carries canonical X.Y.Z only.
    for malformed in ["0.26", "v0.26.0", "0.26.0.1", "01.26.0"] {
        let mut malformed_identity = journal();
        malformed_identity.version = malformed.into();
        assert!(
            malformed_identity.save(&path).is_err(),
            "{malformed:?} must not reach disk"
        );
    }
    let mut malformed_owner = journal();
    malformed_owner.commit = "abcd".into();
    assert!(malformed_owner.save(&path).is_err());
}

/// The website hook's exit contract, pinned: `publish/post-promote` documents
/// 0 synced-or-deferred, 3 no site checkout, 4 live-site lag — and ONLY a code
/// outside that contract (1 hard failure, 2 usage, or a signal) is a failure,
/// which since 2026-09-23 is a loud WARNING and never a parked journal.
#[test]
fn the_site_hook_exit_contract_is_pinned() {
    use publish::{SiteHookOutcome, site_hook_outcome};
    assert_eq!(site_hook_outcome(Some(0)), SiteHookOutcome::Synced);
    assert_eq!(site_hook_outcome(Some(3)), SiteHookOutcome::NoSiteCheckout);
    assert_eq!(site_hook_outcome(Some(4)), SiteHookOutcome::LiveLagging);
    for failing in [Some(1), Some(2), Some(5), Some(127), None] {
        assert_eq!(
            site_hook_outcome(failing),
            SiteHookOutcome::Failed,
            "{failing:?} must be reported as a failure — never pass silently"
        );
    }
}

/// ONE JOURNAL FORMAT (2026-09-23; 10 since 2026-09-24). Every older format — the
/// format-9 file 0.92 left ending in the retired `site` step, the format-8 file
/// 0.91 left parked at it, the v7 one every cut through v0.63.0
/// left, a v1 file with no `format` at all — is refused on load in one sentence
/// that names the file, the cut, and the two ways forward (delete it, or finish it
/// with the cutter that wrote it), instead of being walked against a frozen step
/// list. The negative control is the same done list at the current format, which
/// loads, and an in-flight current journal, which still blocks a fresh cut.
#[test]
fn an_older_journal_format_is_refused_in_one_sentence() {
    let dir = tmpdir("journal-older-format");
    let path = dir.join("cut-state.toml");
    let through_unlock = "\"lock\", \"build\", \"selfcheck\", \"draft\", \"upload\", \
         \"preflip\", \"tag\", \"flip\", \"archive\", \"verify\", \"mirror\", \"unlock\"";
    let write = |format: &str, done: &str| {
        std::fs::write(
            &path,
            format!(
                "{format}version = \"0.91.0\"\nbuild_number = 1790120000\n\
                 commit = \"{}\"\nrelease_id = 55\ndraft_create_issued = true\n\
                 done = [{done}]\n",
                "f".repeat(40)
            ),
        )
        .unwrap();
    };
    for (format, done, named) in [
        // Both format-9 step lists: main's, which kept `site`, and this cutter's
        // first, which had already dropped it — one number, two lists, so neither
        // may reach the prefix check (the_finished_0_92_journal_... below).
        (
            "format = 9\n",
            format!("{through_unlock}, \"site\""),
            "format-9",
        ),
        ("format = 9\n", through_unlock.to_string(), "format-9"),
        (
            "format = 8\n",
            format!("{through_unlock}, \"site\""),
            "format-8",
        ),
        ("format = 8\n", through_unlock.to_string(), "format-8"),
        ("format = 7\n", through_unlock.to_string(), "format-7"),
        ("", "\"build\", \"selfcheck\"".to_string(), "format-1"),
    ] {
        write(format, &done);
        let error = Journal::load(&path)
            .expect_err("an older journal is refused, never walked")
            .to_string();
        assert!(error.contains(named), "{error}");
        assert!(error.contains("v0.91.0 build 1790120000"), "{error}");
        assert!(error.contains("delete it if that cut finished"), "{error}");
        assert!(error.contains("the cutter that wrote it"), "{error}");
        assert_eq!(error.matches(". ").count(), 0, "one sentence: {error}");
    }

    // NEGATIVE CONTROL: the same finished list at the current format loads and is
    // cleared by the next cut; an in-flight one blocks it, by name.
    write(
        &format!("format = {}\n", publish::JOURNAL_FORMAT),
        through_unlock,
    );
    let finished = Journal::load(&path).unwrap().unwrap();
    assert_eq!(finished.first_incomplete(), None);
    assert!(publish::fresh_cut_journal_triage(Some(&finished), publish::CutKind::Real).unwrap());
    write(
        &format!("format = {}\n", publish::JOURNAL_FORMAT),
        "\"lock\", \"build\", \"selfcheck\", \"draft\", \"upload\", \"preflip\", \
           \"tag\", \"flip\", \"archive\", \"verify\"",
    );
    let in_flight = Journal::load(&path).unwrap().unwrap();
    assert_eq!(in_flight.first_incomplete(), Some("mirror"));
    let error = publish::fresh_cut_journal_triage(Some(&in_flight), publish::CutKind::Real)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("already in progress") && error.contains("mirror"),
        "{error}"
    );
    // A retired `site` entry after a current-format list is corruption, not history.
    write(
        &format!("format = {}\n", publish::JOURNAL_FORMAT),
        &format!("{through_unlock}, \"site\""),
    );
    assert!(Journal::load(&path).is_err());
    assert!(!publish::fresh_cut_journal_triage(None, publish::CutKind::Real).unwrap());
}

/// THE JOURNAL v0.92.0 LEFT BEHIND, verbatim (`dist/cut-state.toml` in the checkout
/// that cut it, 2026-09-24). `main`'s cutter wrote it at ITS format 9, which kept
/// the retired `site` step after `unlock` — thirteen entries. This cutter had also
/// numbered its own twelve-step list 9, so the format refusal let the file through
/// and `validate` failed with a prefix error naming neither the file nor a remedy,
/// in front of every cut, dry run, `--abandon` and recover launched from that
/// checkout. Now it gets the one-sentence refusal every retired format gets.
///
/// Negative control: the same finished cut written at the current format with the
/// current step list loads, reads as finished, and is cleared by the next cut —
/// so the refusal is about the number, never the cut.
#[test]
fn the_finished_0_92_journal_main_cut_is_refused_by_name() {
    const V0_92_JOURNAL: &str = r#"format = 9
version = "0.92.0"
build_number = 1790278596
commit = "e8a8c80ab93cdf5bdfd048e22514b663874bb57a"
min_build = 1786131079
arm64_only = false
manifest_signed = true
signature_required = true
signature_pubkey = "YOHw0OoefQ79NdE8qsQFobIMR7QXChpreYBi2Of74Uo="
signature_machine_id = "m3"
release_id = 396027000
draft_create_issued = true
upload_intents = ["aterm-0.92.0.dmg", "aterm-0.92.0.dmg.sha256", "aterm-0.92.0-mac.zip", "aterm-0.92.0-mac.zip.sha256", "aterm-appcast.toml", "aterm-0.92.0-build.txt", "aterm-machines.toml", "aterm-machines.toml.sig", "aterm-appcast.toml.sig", "aterm-0.92.0-dSYM.zip"]
mirror_release_id = 396014794
mirror_create_issued = true
mirror_upload_intents = ["aterm-0.92.0-mac.zip", "aterm-0.92.0-mac.zip.sha256", "aterm-0.92.0.dmg", "aterm-0.92.0.dmg.sha256", "aterm-mac.zip", "aterm-mac.zip.sha256", "aterm-machines.toml", "aterm-machines.toml.sig", "aterm.dmg", "aterm.dmg.sha256", "aterm-appcast.toml", "aterm-appcast.toml.sig"]
done = ["lock", "build", "selfcheck", "draft", "upload", "preflip", "tag", "flip", "archive", "verify", "mirror", "unlock", "site"]
"#;
    assert_ne!(
        publish::JOURNAL_FORMAT,
        9,
        "9 names two step lists; the current format must be a number no other cutter wrote"
    );
    let dir = tmpdir("journal-v0-92");
    let path = dir.join("cut-state.toml");
    std::fs::write(&path, V0_92_JOURNAL).unwrap();
    let error = Journal::load(&path)
        .expect_err("main's finished 0.92 journal is refused, never walked")
        .to_string();
    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(error.contains("format-9 cut journal"), "{error}");
    assert!(error.contains("v0.92.0 build 1790278596"), "{error}");
    assert!(
        error.contains("claim e8a8c80ab93cdf5bdfd048e22514b663874bb57a"),
        "{error}"
    );
    assert!(error.contains("delete it if that cut finished"), "{error}");
    assert!(
        !error.contains("prefix of the canonical pipeline"),
        "{error}"
    );
    assert_eq!(error.matches(". ").count(), 0, "one sentence: {error}");

    // NEGATIVE CONTROL: the same cut at the current format, without the retired
    // step, is a finished journal the next cut clears.
    let current = V0_92_JOURNAL
        .replacen(
            "format = 9\n",
            &format!("format = {}\n", publish::JOURNAL_FORMAT),
            1,
        )
        .replacen(", \"site\"]", "]", 1);
    std::fs::write(&path, current).unwrap();
    let finished = Journal::load(&path)
        .expect("the current format loads")
        .expect("present");
    assert_eq!(finished.version, "0.92.0");
    assert_eq!(finished.done.len(), publish::STEPS.len());
    assert_eq!(finished.first_incomplete(), None);
    assert!(publish::fresh_cut_journal_triage(Some(&finished), publish::CutKind::Real).unwrap());
}

/// A FAILING WEBSITE HOOK LEAVES THE CUT COMPLETE. The hook stub exits 1 (the
/// post-promote contract's "hard failure"); the follow-up reports a WARNING that
/// names the release complete and prints the exact retry command, the journal on
/// disk stays complete, and the next fresh cut is not refused by it. Before
/// 2026-09-23 this exact failure parked the journal at `site` and refused every
/// following cut with "a cut is already in progress".
#[test]
fn a_failing_site_hook_warns_and_leaves_the_journal_complete() {
    let dir = tmpdir("journal-site-hook-fails");
    let path = dir.join("cut-state.toml");
    let mut complete = journal();
    complete.release_id = Some(55);
    complete.draft_create_issued = true;
    complete.done = STEPS.iter().map(|step| (*step).to_string()).collect();
    complete.save(&path).unwrap();

    let hook = dir.join("post-promote");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    let mut ran = Vec::new();
    let follow = publish::site_follows_the_cut(&hook, "0.92.0", &mut |h| {
        ran.push(h.to_path_buf());
        Ok(Some(1))
    });
    assert_eq!(ran, vec![hook.clone()], "the hook is asked once");
    assert_eq!(
        follow,
        publish::SiteFollow::Ran {
            version: "0.92.0".into(),
            outcome: publish::SiteHookOutcome::Failed,
            failure: Some("exit 1".into()),
        }
    );
    let lines = follow.lines().join("\n");
    assert!(lines.contains("WARNING"), "{lines}");
    assert!(lines.contains("COMPLETE"), "{lines}");
    assert!(
        lines.contains("PUB_VERSION=0.92.0 publish/post-promote --latest"),
        "the exact retry command: {lines}"
    );

    let after = Journal::load(&path).unwrap().unwrap();
    assert_eq!(after.first_incomplete(), None, "the journal stays complete");
    assert!(
        publish::fresh_cut_journal_triage(Some(&after), publish::CutKind::Real).unwrap(),
        "and the next cut is not refused by it"
    );

    // A hook that cannot even be started is the same warning, never an error…
    let unstartable = publish::site_follows_the_cut(&hook, "0.92.0", &mut |_| {
        Err(std::io::Error::other("permission denied"))
    });
    assert!(
        unstartable.lines().join("\n").contains("cannot run"),
        "{unstartable:?}"
    );
    // …a success says so, and a tree with no hook skips without running anything.
    let synced = publish::site_follows_the_cut(&hook, "0.92.0", &mut |_| Ok(Some(0)));
    assert!(synced.lines()[0].contains("synced"), "{synced:?}");
    let absent = publish::site_follows_the_cut(&dir.join("absent"), "0.92.0", &mut |_| {
        panic!("an absent hook is never run")
    });
    assert_eq!(absent, publish::SiteFollow::NoHook);
}

/// The public channel's exact asset set carries ONE DMG and refuses the retired
/// `-lite` twin (2026-08-26) as a foreign object, never silently converging it.
#[test]
fn the_exact_asset_set_refuses_the_retired_lite_twin() {
    // Today's exact set has exactly one DMG and no retired name…
    let set = mirror::required_asset_names("0.61.0", false, false);
    assert!(
        !set.iter()
            .any(|n| n.contains("lite") || n.contains("offline") || n.contains("x86_64")),
        "{set:?}"
    );
    mirror::validate_mirror_asset_set_with_linux(&set, "0.61.0", false, false, &[]).unwrap();
    // …and a channel head the old cutter already gave the lean twin to is
    // refused, naming the foreign object.
    let mut stale = set.clone();
    stale.push("aterm-0.61.0-lite.dmg".to_string());
    stale.push("aterm-offline.dmg".to_string());
    let err = mirror::validate_mirror_asset_set_with_linux(&stale, "0.61.0", false, false, &[])
        .expect_err("the retired twin on the channel head is a foreign object");
    assert!(err.to_string().contains("aterm-0.61.0-lite.dmg"), "{err}");
}

#[test]
fn journal_round_trips_and_marks_persist_immediately() {
    let dir = tmpdir("journal");
    let path = dir.join("cut-state.toml");

    // Absent ⇒ Ok(None), never an error (a fresh cut has no journal yet).
    assert!(
        Journal::load(&path)
            .expect("absent journal is fine")
            .is_none()
    );

    let mut j = journal();
    j.min_build = Some(j.build_number);
    j.save(&path).expect("save");
    assert_eq!(
        Journal::load(&path).expect("load").expect("present"),
        j,
        "byte round-trip"
    );

    // mark() persists IMMEDIATELY (journal-every-step) and is idempotent.
    j.mark("lock", &path).expect("mark lock");
    j.mark("build", &path).expect("mark build");
    j.mark("build", &path).expect("mark build again");
    let back = Journal::load(&path).unwrap().unwrap();
    assert_eq!(
        back.done,
        vec!["lock".to_string(), "build".to_string()],
        "no duplicate entries"
    );
    assert_eq!(back.first_incomplete(), Some("selfcheck"));

    // A torn/corrupt journal must STOP resume with an error — never read as
    // "no journal" and silently restart a cut on top of a half-done one.
    std::fs::write(&path, "version = [not toml").unwrap();
    assert!(
        Journal::load(&path).is_err(),
        "corrupt journal must be a hard error"
    );
}

/// Resume treats the journal as artifact authority, so it must never accept a
/// persisted floor that the journaled build itself cannot satisfy.
#[test]
fn journal_rejects_min_build_above_its_claim() {
    let dir = tmpdir("journal-invalid-floor");
    let path = dir.join("cut-state.toml");
    let mut j = journal();
    j.min_build = Some(j.build_number + 1);
    let err = j.save(&path).unwrap_err().to_string();
    assert!(err.contains("exceeds the journaled build"), "{err}");
    assert!(!path.exists(), "an invalid journal must not reach disk");

    // Negative control for a hand-edited journal: load validates the same
    // invariant instead of trusting a syntactically valid TOML record.
    std::fs::write(
        &path,
        format!(
            "format = {}\nversion = \"0.55.0\"\nbuild_number = 550\ncommit = \"{}\"\n\
             min_build = 551\n",
            publish::JOURNAL_FORMAT,
            "a".repeat(40)
        ),
    )
    .unwrap();
    let err = Journal::load(&path).unwrap_err().to_string();
    assert!(err.contains("min_build floor 551"), "{err}");
}

#[test]
fn journal_persists_signature_ratchet_and_actual_key_fail_closed() {
    let dir = tmpdir("journal-signature-policy");
    let path = dir.join("cut-state.toml");
    let mut j = journal();
    j.signature_required = true;
    assert!(
        j.save(&path)
            .unwrap_err()
            .to_string()
            .contains("public key")
    );

    j.signature_pubkey = Some("11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=".into());
    j.save(&path).unwrap();
    assert_eq!(Journal::load(&path).unwrap().unwrap(), j);

    j.done.extend(["lock".into(), "build".into()]);
    let error = j.save(&path).unwrap_err().to_string();
    assert!(
        error.contains("without its required manifest signature"),
        "{error}"
    );
    j.manifest_signed = true;
    j.save(&path).unwrap();

    let mut downgrade = journal();
    downgrade.manifest_signed = true;
    assert!(downgrade.save(&path).is_err());
}

// ---------------------------------------------------------------------------
// pure publish helpers the pipeline steps hang off
// ---------------------------------------------------------------------------

#[test]
fn version_helpers_port_the_shell_derivations() {
    // workspace_version: section-scoped, quote-delimited (the awk's read).
    let cargo = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.25.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n";
    assert_eq!(publish::workspace_version(cargo).unwrap(), "0.25.0");
    // A version key OUTSIDE [workspace.package] must never win.
    let decoy = "[package]\nversion = \"9.9.9\"\n\n[workspace.package]\nversion = \"0.25.0\"\n";
    assert_eq!(publish::workspace_version(decoy).unwrap(), "0.25.0");
    assert!(publish::workspace_version("[workspace]\n").is_err());

    // The workspace version passes through byte-exactly.
    let dev = "[workspace.package]\nversion = \"0.2.1\"\n";
    assert_eq!(publish::workspace_version(dev).unwrap(), "0.2.1");

    // A release IS the workspace version as written, and that is MAJOR.MINOR.0.
    assert_eq!(
        publish::release_version_from_workspace("0.2.0").unwrap(),
        "0.2.0"
    );
    // NEGATIVE CONTROL: a non-zero patch is refused, never rewritten — the binary
    // reports the version as written, so a rewrite would ship an app whose version
    // is not its release's.
    for patched in ["0.2.1", "1.10.7"] {
        let err = publish::release_version_from_workspace(patched)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("a release is MAJOR.MINOR.0") && err.contains("Bump the MINOR"),
            "{patched:?} → {err}"
        );
    }
    for bad in ["0.2", "0.2.1.1", "v0.2.1", "01.2.1", "0.2.x", ""] {
        let err = publish::release_version_from_workspace(bad)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("[workspace.package] version is not canonical MAJOR.MINOR.0"),
            "{bad:?} → {err}"
        );
    }

    // bump_minor_release is numeric, not lexicographic, and stays canonical
    // three-component. It is advisory only: it tells the operator what to
    // bump Cargo.toml to, and no cut ever applies it.
    assert_eq!(publish::bump_minor_release("0.2.0").unwrap(), "0.3.0");
    assert_eq!(publish::bump_minor_release("0.9.0").unwrap(), "0.10.0");
    assert_eq!(
        publish::bump_minor_release("1.9.4").unwrap(),
        "1.10.0",
        "the bump resets the third component"
    );
    assert!(publish::bump_minor_release("nope").is_err());
    assert!(publish::bump_minor_release("0.25").is_err());
    assert!(
        publish::bump_minor_release(&format!("0.{}.0", u64::MAX)).is_err(),
        "MINOR overflow must fail closed, never wrap"
    );

    // The REAL workspace manifest parses, is canonical, and is its own release
    // version.
    let real = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
        .expect("read the real Cargo.toml");
    let v = publish::workspace_version(&real).expect("real manifest must parse");
    assert_eq!(
        v.split('.').count(),
        3,
        "workspace version is X.Y.Z, got {v}"
    );
    let release = publish::release_version_from_workspace(&v)
        .expect("the real workspace version must be canonical MAJOR.MINOR.0");
    assert_eq!(
        release, v,
        "the release version is the workspace version as written"
    );
}

#[test]
fn locked_metadata_gate_rejects_a_workspace_that_would_rewrite_its_lock() {
    let root = tmpdir("locked-metadata");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"lock-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "").unwrap();

    let missing = gates::locked_metadata_gate(&root).unwrap_err().to_string();
    assert!(missing.contains("Cargo.lock"), "{missing}");
    assert!(!root.join("Cargo.lock").exists());

    let stale_lock = "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"lock-gate-fixture\"\nversion = \"0.0.9\"\n";
    std::fs::write(root.join("Cargo.lock"), stale_lock).unwrap();
    let error = gates::locked_metadata_gate(&root).unwrap_err().to_string();
    assert!(error.contains("Cargo.lock"), "{error}");
    assert_eq!(
        std::fs::read_to_string(root.join("Cargo.lock")).unwrap(),
        stale_lock
    );
}

#[test]
fn claim_provenance_binds_the_release_to_the_claims_short_commit() {
    let owner = "0123456789abcdef0123456789abcdef01234567";
    let provenance = b"version=0.59.0\nbuild=1785000000\ncommit=0123456789ab\n";

    publish::validate_claim_provenance(provenance, "0.59.0", 1_785_000_000, owner).unwrap();

    assert!(
        publish::validate_claim_provenance(
            provenance,
            "0.59.0",
            1_785_000_000,
            "fedcba9876543210fedcba9876543210fedcba98"
        )
        .is_err()
    );
    assert!(
        publish::validate_claim_provenance(
            b"version=0.59.0\nbuild=1785000000\ncommit=0123456789ab\ncommit=0123456789ab\n",
            "0.59.0",
            1_785_000_000,
            owner
        )
        .is_err()
    );
}

#[test]
fn repo_slug_parses_the_repository_field_shapes() {
    let mk =
        |url: &str| format!("[workspace.package]\nversion = \"0.25.0\"\nrepository = \"{url}\"\n");
    for url in [
        "https://github.com/alabsystems/aterm",
        "https://github.com/alabsystems/aterm.git",
        "https://github.com/alabsystems/aterm/",
        "git@github.com:alabsystems/aterm.git",
    ] {
        assert_eq!(
            publish::repo_slug(&mk(url)).as_deref(),
            Some("alabsystems/aterm"),
            "{url}"
        );
    }
    assert_eq!(
        publish::repo_slug(&mk("https://example.com/x/y")),
        None,
        "non-GitHub host"
    );
    assert_eq!(
        publish::repo_slug("[workspace.package]\n"),
        None,
        "absent field"
    );
}

/// The monotonic gate of spec §7 steps 4/5, including the resume subtlety: a
/// live release at exactly our build under OUR tag is this very cut,
/// half-flipped by a crashed attempt — resuming past it must not self-abort.
#[test]
fn monotonic_gate_accepts_only_strictly_newer_or_our_own_cut() {
    let ok = |n, best| publish::monotonic_ok(n, "v0.26.0", best).is_ok();
    assert!(ok(100, None), "an empty repo cannot outrank us");
    assert!(ok(100, Some(("v0.25.0", 99))), "strictly newer wins");
    assert!(
        ok(100, Some(("v0.26.0", 100))),
        "our own half-flipped release is fine"
    );
    assert!(
        !ok(100, Some(("v0.99.0", 100))),
        "same build under another tag is a collision"
    );
    assert!(
        !ok(100, Some(("v0.99.0", 101))),
        "an older n than live must abort"
    );
    let err = publish::monotonic_ok(100, "v0.26.0", Some(("v0.99.0", 101))).unwrap_err();
    assert!(err.to_string().contains("monotonic"), "{err}");
}

/// The channel floor is state, not a one-shot flag: every successor takes the
/// maximum of operator input and the newest visible manifest. Zero is
/// canonicalized back to absence so an unratcheted channel stays byte-clean.
#[test]
fn effective_min_build_is_monotonic_canonical_and_bounded_by_claim() {
    let resolve = |operator, channel| publish::effective_min_build(operator, channel, 100).unwrap();
    assert_eq!(resolve(None, None), None);
    assert_eq!(resolve(Some(0), None), None);
    assert_eq!(resolve(None, Some(0)), None);
    assert_eq!(resolve(Some(40), None), Some(40));
    assert_eq!(resolve(None, Some(70)), Some(70));
    assert_eq!(resolve(Some(40), Some(70)), Some(70));
    assert_eq!(resolve(Some(90), Some(70)), Some(90));
    assert_eq!(resolve(Some(100), Some(99)), Some(100));

    let err = publish::effective_min_build(Some(101), Some(70), 100)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("min_build floor 101") && err.contains("newly claimed build 100"),
        "{err}"
    );

    // Negative control for the retired operator-only policy: this fixture is
    // non-vacuous because dropping the channel input would lower 70 to 40.
    let retired_operator_only = Some(40);
    assert_ne!(retired_operator_only, resolve(Some(40), Some(70)));
}

/// Exhaustive bounded conformance over the REAL shipping resolver. This is the
/// finite-state proof obligation for the carry-forward transition: across every
/// absent/present floor through 4 and every claim through 4, the output is
/// exactly canonical max(input), never decreases either input, and is accepted
/// iff the claimed build can satisfy it. The operator-only mutant must have a
/// reachable counterexample so the proof cannot pass vacuously.
#[test]
fn bounded_floor_state_space_proves_carry_forward_invariants_and_catches_mutant() {
    let floors: Vec<Option<u64>> = std::iter::once(None).chain((0..=4).map(Some)).collect();
    let mut mutant_caught = false;

    for operator in &floors {
        for channel in &floors {
            for claimed in 0..=4 {
                let raw_max = operator.unwrap_or(0).max(channel.unwrap_or(0));
                let expected = (raw_max != 0).then_some(raw_max);
                let got = publish::effective_min_build(*operator, *channel, claimed);
                if raw_max > claimed {
                    assert!(got.is_err(), "({operator:?}, {channel:?}, {claimed})");
                } else {
                    let got = got.unwrap();
                    assert_eq!(got, expected, "({operator:?}, {channel:?}, {claimed})");
                    assert!(got.unwrap_or(0) >= operator.unwrap_or(0));
                    assert!(got.unwrap_or(0) >= channel.unwrap_or(0));
                    assert!(got.unwrap_or(0) <= claimed);
                }

                let retired = operator.filter(|floor| *floor != 0);
                if raw_max <= claimed && retired != expected {
                    mutant_caught = true;
                }
            }
        }
    }
    assert!(
        mutant_caught,
        "bounded domain must distinguish carry-forward from operator-only policy"
    );
}

/// A second scan immediately before visibility closes the race between the
/// initial carry-forward decision and another publisher raising the channel.
#[test]
fn late_channel_floor_guard_refuses_a_ratcheting_race() {
    assert!(publish::channel_floor_covered(None, None).is_ok());
    assert!(publish::channel_floor_covered(Some(70), Some(70)).is_ok());
    assert!(publish::channel_floor_covered(Some(90), Some(70)).is_ok());
    let err = publish::channel_floor_covered(Some(70), Some(71))
        .unwrap_err()
        .to_string();
    assert!(err.contains("advanced to min_build 71"), "{err}");
}

#[test]
fn origin_url_parser_accepts_only_unambiguous_exact_github_repo_forms() {
    for url in [
        "https://github.com/alabsystems/aterm.git",
        "git@github.com:alabsystems/aterm.git",
        "ssh://git@github.com/alabsystems/aterm.git",
    ] {
        assert_eq!(
            publish::github_slug_from_remote_url(url).unwrap(),
            "alabsystems/aterm"
        );
    }
    for bad in [
        "https://evil.example/alabsystems/aterm.git",
        "https://github.com/alabsystems/aterm/extra",
        "https://github.com/alabsystems",
        "https://github.com/alabsystems/aterm?other=true",
    ] {
        assert!(publish::github_slug_from_remote_url(bad).is_err(), "{bad}");
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty(), "odd-length hex test vector");
    pairs
        .iter()
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("non-hex test vector"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}

#[test]
fn detached_signature_verifier_matches_rfc8032_and_catches_mutations() {
    // RFC 8032 Ed25519 test vector 1: empty message.
    let pubkey = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    let signature = decode_hex(concat!(
        "e5564300c360ac729086e2cc806e828a",
        "84877f1eb8e5d974d873e06522490155",
        "5fb8821590a33bacc61e39701cf9b46b",
        "d25bf5f0595bbe24655141438e7a100b"
    ));
    publish::verify_detached_manifest_signature(pubkey, b"", &signature).unwrap();
    assert!(publish::verify_detached_manifest_signature(pubkey, b"x", &signature).is_err());
    let mut mutated = signature.clone();
    mutated[0] ^= 1;
    assert!(publish::verify_detached_manifest_signature(pubkey, b"", &mutated).is_err());
    assert!(publish::canonical_update_pubkey("not-base64").is_err());
}

#[test]
fn release_asset_digest_replay_uses_the_updater_download_bound() {
    assert!(publish::validate_release_asset_download_size(1).is_ok());
    assert!(publish::validate_release_asset_download_size(2_147_483_648).is_ok());
    assert!(publish::validate_release_asset_download_size(0).is_err());
    assert!(publish::validate_release_asset_download_size(2_147_483_649).is_err());
    assert_eq!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_ASSET, 262_144).unwrap(),
        262_144
    );
    assert!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_ASSET, 262_145).is_err()
    );
    assert_eq!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_SIG_ASSET, 64).unwrap(),
        64
    );
    assert!(
        publish::validate_small_release_asset_size(manifest_out::MANIFEST_SIG_ASSET, 63).is_err()
    );
    assert!(publish::validate_small_release_asset_size("aterm-0.55.0.dmg", 1).is_err());
    assert_eq!(
        publish::read_bounded_release_asset(std::io::Cursor::new(vec![7_u8; 64]), 64).unwrap(),
        vec![7_u8; 64]
    );
    assert!(
        publish::read_bounded_release_asset(std::io::Cursor::new(vec![7_u8; 65]), 64).is_err(),
        "a response that grows after metadata preflight stays memory-bounded"
    );
    let (diagnostic, truncated) =
        publish::drain_bounded_diagnostic(std::io::Cursor::new(vec![b'e'; 1024 * 1024]), 1024)
            .unwrap();
    assert_eq!(diagnostic.len(), 1024);
    assert!(
        truncated,
        "noisy stderr is drained but retained only to cap"
    );

    let mut exact_sink = Vec::new();
    let (size, digest) = publish::copy_bounded_release_asset(
        std::io::Cursor::new(b"exact bytes"),
        &mut exact_sink,
        11,
    )
    .unwrap();
    assert_eq!(size, 11);
    assert_eq!(exact_sink, b"exact bytes");
    assert_eq!(
        digest,
        "e38e581aade78b64cc86f7ac9f3555ca78c2dcca747942a7f1d9b3275a834f75"
    );

    let mut bounded_sink = Vec::new();
    let error = publish::copy_bounded_release_asset(
        std::io::Cursor::new(vec![9_u8; 1025]),
        &mut bounded_sink,
        1024,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("1024-byte transfer bound"), "{error}");
    assert!(
        bounded_sink.len() <= 1024,
        "the over-limit probe byte must never reach disk"
    );

    assert_eq!(
        publish::parse_release_asset_identity_rows("", "v0.55.0", "asset.toml").unwrap(),
        None
    );
    assert_eq!(
        publish::parse_release_asset_identity_rows(
            "other.txt\t1\t7\nasset.toml\t42\t64\n",
            "v0.55.0",
            "asset.toml",
        )
        .unwrap(),
        Some((42, 64))
    );
    let duplicate = publish::parse_release_asset_identity_rows(
        "asset.toml\t42\t64\nasset.toml\t43\t64\n",
        "v0.55.0",
        "asset.toml",
    )
    .unwrap_err()
    .to_string();
    assert!(duplicate.contains("2 assets"), "{duplicate}");
    assert!(
        publish::parse_release_asset_identity_rows("asset.toml\t42\t0\n", "v0.55.0", "asset.toml",)
            .is_err(),
        "an empty exact-ID object is never treated as an absent optional asset"
    );
}

#[test]
fn production_release_asset_reads_never_use_name_based_downloads() {
    let sources = concat!(
        include_str!("../src/publish.rs"),
        include_str!("../src/verify.rs")
    );
    let tokens = sources.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        !tokens.contains("\"release\", \"download\""),
        "all release reads must resolve one exact name→ID and use the bounded asset-ID API"
    );
}

#[test]
fn live_signature_replay_requires_exact_unique_valid_head_without_fallback() {
    let pubkey = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    let signature = decode_hex(concat!(
        "e5564300c360ac729086e2cc806e828a",
        "84877f1eb8e5d974d873e06522490155",
        "5fb8821590a33bacc61e39701cf9b46b",
        "d25bf5f0595bbe24655141438e7a100b"
    ));
    let head = archive_release(
        "v0.55.0",
        false,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, manifest_out::MANIFEST_SIG_ASSET),
        ],
    );
    let verify = |releases: &[publish::AppcastRelease], bytes: Vec<u8>| {
        publish::verify_channel_head_signature_with(
            releases,
            "v0.55.0",
            b"",
            Some(&signature),
            Some(pubkey),
            |_, _, tag, name| {
                assert_eq!(tag, "v0.55.0");
                assert_eq!(name, manifest_out::MANIFEST_SIG_ASSET);
                Ok(bytes.clone())
            },
        )
    };
    assert!(verify(std::slice::from_ref(&head), signature.clone()).unwrap());

    let missing = archive_release("v0.55.0", false, &[(1, manifest_out::MANIFEST_ASSET)]);
    // With no signature and no trusted pin the legacy channel is unsigned.
    assert!(
        !publish::verify_channel_head_signature_with(
            &[missing],
            "v0.55.0",
            b"",
            None,
            None,
            |_, _, _, _| unreachable!(),
        )
        .unwrap()
    );
    // Once a key is pinned, deleting every remote signature must not reset the
    // verifier to unsigned: deployed updaters would reject these bytes.
    assert!(
        publish::verify_channel_head_signature_with(
            &[archive_release(
                "v0.55.0",
                false,
                &[(1, manifest_out::MANIFEST_ASSET)],
            )],
            "v0.55.0",
            b"",
            None,
            Some(pubkey),
            |_, _, _, _| unreachable!(),
        )
        .is_err()
    );

    let duplicate = archive_release(
        "v0.55.0",
        false,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, manifest_out::MANIFEST_SIG_ASSET),
            (3, manifest_out::MANIFEST_SIG_ASSET),
        ],
    );
    assert!(verify(&[duplicate], signature.clone()).is_err());

    let archive_fallback = archive_release(
        "v0.55.0",
        false,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, "aterm-appcast-v0.55.0.toml.sig"),
        ],
    );
    assert!(verify(&[archive_fallback], signature.clone()).is_err());

    let mut corrupt = signature.clone();
    corrupt[0] ^= 1;
    assert!(verify(&[head], corrupt).is_err());
}

#[test]
fn live_archive_identity_requires_exact_version_build_commit_and_local_bytes() {
    let commit = "a".repeat(40);
    let manifest = format!(
        "schema = 1\nversion = \"0.55.0\"\nbuild_number = 55\ncommit = \"{commit}\"\n\
         dmg = \"aterm-0.55.0.dmg\"\nsha256 = \"{}\"\n",
        "0".repeat(64)
    );
    let expected = publish::ExpectedReleaseIdentity {
        version: "0.55.0",
        build: 55,
        commit: &commit,
    };
    publish::validate_live_release_identity(
        expected,
        manifest.as_bytes(),
        None,
        Some(manifest.as_bytes()),
        None,
        false,
        None,
    )
    .unwrap();

    for bad in [
        manifest.replace("version = \"0.55.0\"", "version = \"0.56.0\""),
        manifest.replace("build_number = 55", "build_number = 56"),
        manifest.replace(&commit, &"b".repeat(40)),
        manifest.replace("aterm-0.55.0.dmg", "other.dmg"),
    ] {
        assert!(
            publish::validate_live_release_identity(
                expected,
                bad.as_bytes(),
                None,
                None,
                None,
                false,
                None,
            )
            .is_err()
        );
    }
    let err = publish::validate_live_release_identity(
        expected,
        manifest.as_bytes(),
        None,
        Some(b"different bytes"),
        None,
        false,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("byte-identical"), "{err}");
}

// ---------------------------------------------------------------------------
// journaled single-head appcast archive migration
// ---------------------------------------------------------------------------

fn archive_release(tag: &str, draft: bool, assets: &[(u64, &str)]) -> publish::AppcastRelease {
    publish::AppcastRelease {
        release_id: assets.first().map_or(1, |(id, _)| *id),
        tag: tag.to_string(),
        draft,
        target_commitish: "a".repeat(40),
        assets: assets
            .iter()
            .map(|(id, name)| publish::AppcastAsset {
                id: *id,
                name: (*name).to_string(),
            })
            .collect(),
    }
}

#[derive(Debug)]
struct FakeArchiveRemote {
    releases: Vec<publish::AppcastRelease>,
    renamed: Vec<publish::AppcastRename>,
    /// Fail once just before this zero-based successful rename count.
    fail_after: Option<usize>,
    /// Retired mutant: claim PATCH success without changing metadata.
    no_archive_mutant: bool,
}

impl FakeArchiveRemote {
    fn new(releases: Vec<publish::AppcastRelease>) -> Self {
        Self {
            releases,
            renamed: Vec::new(),
            fail_after: None,
            no_archive_mutant: false,
        }
    }

    fn asset_name(&self, id: u64) -> Option<&str> {
        self.releases
            .iter()
            .flat_map(|release| &release.assets)
            .find(|asset| asset.id == id)
            .map(|asset| asset.name.as_str())
    }
}

impl publish::AppcastArchiveRemote for FakeArchiveRemote {
    fn list_releases(&mut self) -> ledger::Result<Vec<publish::AppcastRelease>> {
        Ok(self.releases.clone())
    }

    fn rename_asset(&mut self, rename: &publish::AppcastRename) -> ledger::Result<()> {
        if self.fail_after == Some(self.renamed.len()) {
            self.fail_after = None;
            return Err(ledger::Error::new("injected crash between PATCHes"));
        }
        self.renamed.push(rename.clone());
        if self.no_archive_mutant {
            return Ok(());
        }
        let release = self
            .releases
            .iter_mut()
            .find(|release| !release.draft && release.tag == rename.tag)
            .ok_or_else(|| ledger::Error::new("fake release missing"))?;
        let asset = release
            .assets
            .iter_mut()
            .find(|asset| asset.id == rename.id)
            .ok_or_else(|| ledger::Error::new("fake asset missing"))?;
        if asset.name != rename.from {
            return Err(ledger::Error::new(format!(
                "fake source drifted: {:?} != {:?}",
                asset.name, rename.from
            )));
        }
        asset.name.clone_from(&rename.to);
        Ok(())
    }
}

fn archive_fixture() -> Vec<publish::AppcastRelease> {
    vec![
        archive_release(
            "v0.55.0",
            false,
            &[
                (1, manifest_out::MANIFEST_ASSET),
                (2, manifest_out::MANIFEST_SIG_ASSET),
            ],
        ),
        archive_release(
            "v0.54.0",
            false,
            &[
                (3, manifest_out::MANIFEST_ASSET),
                (4, manifest_out::MANIFEST_SIG_ASSET),
            ],
        ),
        archive_release("v0.53.0", false, &[(5, manifest_out::MANIFEST_ASSET)]),
        archive_release(
            "v0.52.0",
            false,
            &[
                (6, "aterm-appcast-v0.52.0.toml"),
                (7, "aterm-appcast-v0.52.0.toml.sig"),
            ],
        ),
        // Drafts are outside the channel and remain byte/name untouched, even
        // if they carry exact names that would collide if published.
        archive_release(
            "v0.56.0",
            true,
            &[
                (8, manifest_out::MANIFEST_ASSET),
                (9, manifest_out::MANIFEST_SIG_ASSET),
                (10, "aterm-appcast-v0.56.0.toml"),
            ],
        ),
    ]
}

#[test]
fn signing_is_never_required_by_history_but_metadata_stays_coherent() {
    // The ratchet is retired (Tier REPO): published `.sig` history — exact OR
    // archived — never forces a signed successor. `channel_signature_required`
    // is unconditionally false, but still surfaces incoherent metadata.
    let unsigned = vec![archive_release(
        "v0.55.0",
        false,
        &[(1, manifest_out::MANIFEST_ASSET)],
    )];
    assert!(!publish::channel_signature_required(&unsigned).unwrap());

    let exact = vec![archive_release(
        "v0.26.0",
        false,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, manifest_out::MANIFEST_SIG_ASSET),
        ],
    )];
    assert!(
        !publish::channel_signature_required(&exact).unwrap(),
        "an exact `.sig` in history no longer demands a signed successor"
    );

    let archived = vec![archive_release(
        "v0.26.0",
        false,
        &[
            (1, "aterm-appcast-v0.26.0.toml"),
            (2, "aterm-appcast-v0.26.0.toml.sig"),
        ],
    )];
    assert!(
        !publish::channel_signature_required(&archived).unwrap(),
        "archived `.sig` history no longer demands a signed successor"
    );

    let draft_only = vec![archive_release(
        "v0.56.0",
        true,
        &[
            (1, manifest_out::MANIFEST_ASSET),
            (2, manifest_out::MANIFEST_SIG_ASSET),
        ],
    )];
    assert!(!publish::channel_signature_required(&draft_only).unwrap());

    // Incoherent signed-asset metadata (a signature with no paired manifest) is
    // still a hard error so the archive planner sees a consistent inventory.
    let orphan = vec![archive_release(
        "v0.26.0",
        false,
        &[(2, "aterm-appcast-v0.26.0.toml.sig")],
    )];
    assert!(publish::channel_signature_required(&orphan).is_err());
}

/// A valid Ed25519 key (the RFC 8032 vector also used above).
const OTHER_KEY: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";

#[test]
fn without_a_paper_master_signing_stays_per_machine_opt_in() {
    // Forks and private channels pin no master, so their behavior is per-machine
    // opt-in: keyless cuts unsigned, a configured key signs. (With the master pinned
    // — this tree — a keyless cut refuses pre-claim; `machine_roster.rs` pins that.)
    let unsigned = publish::unrostered_signature_policy(None).unwrap();
    assert!(!unsigned.required);
    assert_eq!(unsigned.pubkey, None);

    let opted_in = publish::unrostered_signature_policy(Some(OTHER_KEY)).unwrap();
    assert!(opted_in.required);
    assert_eq!(opted_in.pubkey.as_deref(), Some(OTHER_KEY));
}

#[test]
fn unsigned_successor_is_allowed_even_when_archived_signatures_exist() {
    // Killed ratchet: an unsigned v0.55.0 head archives cleanly alongside a prior
    // release that still carries archived `.sig` bytes. No signed head demanded.
    let releases = vec![
        archive_release("v0.55.0", false, &[(1, manifest_out::MANIFEST_ASSET)]),
        archive_release(
            "v0.26.0",
            false,
            &[
                (2, "aterm-appcast-v0.26.0.toml"),
                (3, "aterm-appcast-v0.26.0.toml.sig"),
            ],
        ),
    ];
    let plan = publish::plan_appcast_archive(&releases, "v0.55.0")
        .expect("unsigned successor is always permitted");
    assert!(
        plan.is_empty(),
        "already-archived history needs no further renames: {plan:?}"
    );
}

/// Happy-path conformance against the injected executor: every historical
/// exact manifest/signature is renamed in place, already-archived history and
/// drafts are untouched, and asset IDs prove bytes were preserved.
#[test]
fn archive_converges_to_one_exact_head_with_deterministic_reversible_renames() {
    let mut remote = FakeArchiveRemote::new(archive_fixture());
    let renamed = publish::converge_appcast_archive(&mut remote, "v0.55.0").unwrap();
    assert_eq!(renamed, 3);
    assert_eq!(
        remote
            .renamed
            .iter()
            .map(|rename| (rename.id, rename.from.as_str(), rename.to.as_str()))
            .collect::<Vec<_>>(),
        [
            (3, "aterm-appcast.toml", "aterm-appcast-v0.54.0.toml"),
            (
                4,
                "aterm-appcast.toml.sig",
                "aterm-appcast-v0.54.0.toml.sig"
            ),
            (5, "aterm-appcast.toml", "aterm-appcast-v0.53.0.toml"),
        ]
    );
    assert_eq!(remote.asset_name(1), Some(manifest_out::MANIFEST_ASSET));
    assert_eq!(remote.asset_name(3), Some("aterm-appcast-v0.54.0.toml"));
    assert_eq!(remote.asset_name(4), Some("aterm-appcast-v0.54.0.toml.sig"));
    assert_eq!(remote.asset_name(6), Some("aterm-appcast-v0.52.0.toml"));
    assert_eq!(remote.asset_name(8), Some(manifest_out::MANIFEST_ASSET));
    publish::prove_single_appcast_head(&remote.releases, "v0.55.0").unwrap();
}

/// Journal-level partial resume: the first metadata PATCH survives an injected
/// crash, and the next convergence plans only the unrenamed suffix. No asset is
/// downloaded, replaced, or renamed twice.
#[test]
fn archive_partial_patch_resumes_from_remote_metadata() {
    let mut remote = FakeArchiveRemote::new(vec![
        archive_release(
            "v0.55.0",
            false,
            &[
                (1, manifest_out::MANIFEST_ASSET),
                (2, manifest_out::MANIFEST_SIG_ASSET),
            ],
        ),
        archive_release(
            "v0.54.0",
            false,
            &[
                (3, manifest_out::MANIFEST_ASSET),
                (4, manifest_out::MANIFEST_SIG_ASSET),
            ],
        ),
    ]);
    remote.fail_after = Some(1);
    let err = publish::converge_appcast_archive(&mut remote, "v0.55.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("injected crash"), "{err}");
    assert_eq!(remote.asset_name(3), Some("aterm-appcast-v0.54.0.toml"));
    assert_eq!(remote.asset_name(4), Some(manifest_out::MANIFEST_SIG_ASSET));

    assert_eq!(
        publish::converge_appcast_archive(&mut remote, "v0.55.0").unwrap(),
        1,
        "resume performs only the unfinished signature PATCH"
    );
    assert_eq!(
        remote
            .renamed
            .iter()
            .map(|rename| rename.id)
            .collect::<Vec<_>>(),
        [3, 4],
        "the successful prefix is never repeated"
    );
}

/// Collision preflight covers the entire release set before mutation, so an
/// existing deterministic target can never be overwritten or partially mixed
/// with earlier successful renames.
#[test]
fn archive_name_collision_fails_before_any_patch() {
    let mut releases = archive_fixture();
    // Put the collision after two earlier releases that would otherwise plan
    // three renames; whole-set planning must still execute zero PATCHes.
    releases[3].assets.push(publish::AppcastAsset {
        id: 30,
        name: manifest_out::MANIFEST_ASSET.into(),
    });
    let mut remote = FakeArchiveRemote::new(releases);
    let err = publish::converge_appcast_archive(&mut remote, "v0.55.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("name collision"), "{err}");
    assert!(
        remote.renamed.is_empty(),
        "collision must preflight globally"
    );
    assert_eq!(remote.asset_name(3), Some(manifest_out::MANIFEST_ASSET));
}

/// Negative control for the retired no-archive behavior: even an executor that
/// falsely reports PATCH success cannot pass the postcondition without actually
/// moving the same asset IDs to deterministic archive names.
#[test]
fn no_archive_mutant_is_caught_by_fresh_remote_proof() {
    let mut remote = FakeArchiveRemote::new(archive_fixture());
    remote.no_archive_mutant = true;
    let err = publish::converge_appcast_archive(&mut remote, "v0.55.0")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("after PATCH") || err.contains("single-head invariant"),
        "{err}"
    );
    assert_eq!(remote.asset_name(3), Some(manifest_out::MANIFEST_ASSET));
}

/// The exact discovery invariant is independently executable and includes the
/// draft boundary plus the paired-signature postcondition.
#[test]
fn exactly_one_head_invariant_accepts_only_current_published_exact_name() {
    let converged = vec![
        archive_release(
            "v0.55.0",
            false,
            &[
                (1, manifest_out::MANIFEST_ASSET),
                (4, manifest_out::MANIFEST_SIG_ASSET),
            ],
        ),
        archive_release("v0.54.0", false, &[(2, "aterm-appcast-v0.54.0.toml")]),
        archive_release("v0.56.0", true, &[(3, manifest_out::MANIFEST_ASSET)]),
    ];
    publish::prove_single_appcast_head(&converged, "v0.55.0").unwrap();

    let mut two_heads = converged.clone();
    two_heads[1].assets[0].name = manifest_out::MANIFEST_ASSET.into();
    assert!(publish::prove_single_appcast_head(&two_heads, "v0.55.0").is_err());

    let mut no_head = converged.clone();
    no_head[0].assets[0].name = "aterm-appcast-v0.55.0.toml".into();
    assert!(publish::prove_single_appcast_head(&no_head, "v0.55.0").is_err());

    let mut stale_sig = converged;
    stale_sig[1].assets.push(publish::AppcastAsset {
        id: 5,
        name: manifest_out::MANIFEST_SIG_ASSET.into(),
    });
    assert!(publish::prove_single_appcast_head(&stale_sig, "v0.55.0").is_err());
}

/// A resumed old cut is never allowed to rename a newer live head. This is
/// independent of GitHub list order: the vMAJOR.MINOR.PATCH channel protocol
/// is the authority, and the entire plan fails before the first PATCH.
#[test]
fn stale_archive_refuses_newer_exact_head_before_any_patch() {
    let mut releases = archive_fixture();
    releases[4].draft = false;
    let mut remote = FakeArchiveRemote::new(releases);
    let err = publish::converge_appcast_archive(&mut remote, "v0.55.0")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("same-or-newer published channel tag v0.56.0"),
        "{err}"
    );
    assert!(remote.renamed.is_empty(), "stale plan must mutate nothing");
    assert_eq!(remote.asset_name(3), Some(manifest_out::MANIFEST_ASSET));
    assert_eq!(remote.asset_name(8), Some(manifest_out::MANIFEST_ASSET));

    let model = aterm_spec::derive::release_channel_single_head_model();
    let mut stale = model.init_state();
    for action in [
        "LoadUnfinishedLegacyJournal",
        "AcquireCompetingOwner",
        "PublishNewerHead",
    ] {
        assert!(model.fire(action, &mut stale), "{action}: {stale:?}");
    }
    let refused = model.successors("AbortNewerHead", &stale)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &stale,
        &refused,
        Some("AbortNewerHead"),
        "archive planner refuses a newer exact channel head before PATCH",
    );
    assert!(
        admitted,
        "model rejected production stale-head refusal: {why}"
    );
}

/// THE first-cut hazard of the cut-over, executable: the live channel head is
/// the retired two-component v0.61, and the first release under the new scheme
/// is v0.2.0 — which orders BELOW it on any two-field comparison. A retired
/// release is not on this version line at all, so it cannot contest authority;
/// it is still archived off the client's discovery surface like any history.
#[test]
fn archive_authority_ignores_retired_two_component_releases() {
    let current = archive_release("v0.2.0", false, &[(1, manifest_out::MANIFEST_ASSET)]);
    let retired = archive_release("v0.61", false, &[(2, manifest_out::MANIFEST_ASSET)]);
    let plan = publish::plan_appcast_archive(&[current, retired], "v0.2.0")
        .expect("a retired release cannot block the first cut under the new scheme");
    assert_eq!(
        plan.iter()
            .map(|rename| (rename.tag.as_str(), rename.to.as_str()))
            .collect::<Vec<_>>(),
        [("v0.61", "aterm-appcast-v0.61.toml")],
        "the retired head still loses its exact name"
    );

    // Convergence proves the single-head invariant against a real remote.
    let mut remote = FakeArchiveRemote::new(vec![
        archive_release("v0.2.0", false, &[(1, manifest_out::MANIFEST_ASSET)]),
        archive_release("v0.61", false, &[(2, manifest_out::MANIFEST_ASSET)]),
        archive_release("v0.25", false, &[(3, "aterm-appcast-v0.25.toml")]),
    ]);
    assert_eq!(
        publish::converge_appcast_archive(&mut remote, "v0.2.0").unwrap(),
        1,
        "already-archived retired history needs no further renames"
    );
    assert_eq!(remote.asset_name(1), Some(manifest_out::MANIFEST_ASSET));
    assert_eq!(remote.asset_name(2), Some("aterm-appcast-v0.61.toml"));
    publish::prove_single_appcast_head(&remote.releases, "v0.2.0").unwrap();
}

/// The archive planner orders by the full three-component tag: the
/// repository's real pre-canonical `v0.21.2607041853` shape is provably older
/// history, while a same/newer PATCH extension is never mistaken for it.
#[test]
fn archive_orders_deep_numeric_tags_without_weakening_stale_head_guard() {
    let current = archive_release("v0.55.0", false, &[(1, manifest_out::MANIFEST_ASSET)]);
    let older = archive_release(
        "v0.21.2607041853",
        false,
        &[(2, manifest_out::MANIFEST_ASSET)],
    );
    let plan = publish::plan_appcast_archive(&[current.clone(), older], "v0.55.0")
        .expect("a lower three-component tag is provably older");
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].tag, "v0.21.2607041853");

    let future = archive_release("v0.55.1", false, &[(3, manifest_out::MANIFEST_ASSET)]);
    let err = publish::plan_appcast_archive(&[current, future], "v0.55.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("same-or-newer"), "{err}");

    let model = aterm_spec::derive::release_channel_single_head_model();
    let mut wrong_tag = model.init_state();
    for action in [
        "LoadUnfinishedLegacyJournal",
        "AcquireCompetingOwner",
        "ReplaceTagAtSameBuild",
    ] {
        assert!(
            model.fire(action, &mut wrong_tag),
            "{action}: {wrong_tag:?}"
        );
    }
    let refused = model.successors("AbortWrongTag", &wrong_tag)[0].clone();
    let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &wrong_tag,
        &refused,
        Some("AbortWrongTag"),
        "archive planner refuses a same-build noncanonical successor tag",
    );
    assert!(
        admitted,
        "model rejected production wrong-tag refusal: {why}"
    );
}

/// Killed ratchet: an unsigned current head archives cleanly even when the
/// prior fixture history carried signatures. Nothing forces a signed head, so
/// the whole migration converges to a single unsigned exact head.
#[test]
fn unsigned_current_head_archives_cleanly_alongside_signed_history() {
    let mut releases = archive_fixture();
    releases[0]
        .assets
        .retain(|asset| asset.name != manifest_out::MANIFEST_SIG_ASSET);
    let mut remote = FakeArchiveRemote::new(releases);
    let renamed = publish::converge_appcast_archive(&mut remote, "v0.55.0")
        .expect("unsigned successor is always permitted");
    assert_eq!(renamed, 3);
    publish::prove_single_appcast_head(&remote.releases, "v0.55.0").unwrap();

    // The retired caller-supplied bool cannot re-arm a ratchet either: planning
    // the same converged unsigned head again is a no-op, never a refusal.
    assert!(
        publish::plan_appcast_archive(&remote.releases, "v0.55.0")
            .unwrap()
            .is_empty()
    );
}

/// Production listing parser retains exact and archived IDs under their
/// deterministic names and represents asset-less releases for pagination.
#[test]
fn archive_listing_parser_is_lossless_for_relevant_metadata() {
    let rows = r#"{"release_id":55,"tag":"v0.55.0","draft":false,"target_commitish":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","assets":[{"id":1,"name":"aterm-appcast.toml"},{"id":2,"name":"aterm-appcast.toml.sig"}]}
{"release_id":54,"tag":"v0.54.0","draft":false,"target_commitish":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","assets":[{"id":3,"name":"aterm-appcast-v0.54.0.toml"},{"id":4,"name":"aterm-appcast-v0.54.0.toml.sig"}]}
{"release_id":56,"tag":"v0.56.0","draft":true,"target_commitish":"cccccccccccccccccccccccccccccccccccccccc","assets":[{"id":5,"name":"aterm-appcast.toml"}]}
"#;
    let parsed = publish::parse_appcast_asset_listing(rows).unwrap();
    assert_eq!(parsed.len(), 3);
    assert_eq!(
        parsed[0].assets,
        [
            publish::AppcastAsset {
                id: 1,
                name: manifest_out::MANIFEST_ASSET.into(),
            },
            publish::AppcastAsset {
                id: 2,
                name: manifest_out::MANIFEST_SIG_ASSET.into(),
            },
        ]
    );
    assert_eq!(
        parsed[1].assets,
        [
            publish::AppcastAsset {
                id: 3,
                name: "aterm-appcast-v0.54.0.toml".into(),
            },
            publish::AppcastAsset {
                id: 4,
                name: "aterm-appcast-v0.54.0.toml.sig".into(),
            },
        ]
    );
    assert!(parsed[2].draft);
    assert!(publish::parse_appcast_asset_listing("broken row\n").is_err());
}

/// Production's JSON projection retains every matching asset instead of
/// collapsing `[0]`; duplicate exact names therefore reach the same ambiguity
/// guard exercised by the in-memory remote.
#[test]
fn archive_listing_preserves_duplicates_for_fail_closed_preflight() {
    let rows = r#"{"release_id":55,"tag":"v0.55.0","draft":false,"target_commitish":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","assets":[{"id":1,"name":"aterm-appcast.toml"},{"id":2,"name":"aterm-appcast.toml"},{"id":3,"name":"aterm-appcast.toml.sig"}]}
"#;
    let parsed = publish::parse_appcast_asset_listing(rows).unwrap();
    assert_eq!(parsed[0].assets.len(), 3);
    let err = publish::plan_appcast_archive(&parsed, "v0.55.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("duplicate assets"), "{err}");
}

/// Status exhaustively scans every historical manifest, then reports the
/// highest build; a tie keeps the first API-order entry. This helper is NOT the
/// updater's live choice, which resolves one canonical tag authority first.
#[test]
fn select_newest_summarizes_an_exhaustive_status_scan() {
    let p = |tag: &str, build: u64| verify::Published {
        release_id: Some(build),
        release: None,
        tag: tag.into(),
        build,
        version: tag.trim_start_matches('v').into(),
        asset: manifest_out::MANIFEST_ASSET.into(),
        min_build: None,
        text: String::new(),
    };
    assert!(verify::select_newest(&[]).is_none());
    let scanned = [p("v0.26.0", 200), p("v0.25.0", 100), p("v0.24.0-dup", 200)];
    let best = verify::select_newest(&scanned).unwrap();
    assert_eq!(
        (best.tag.as_str(), best.build),
        ("v0.26.0", 200),
        "first max wins the tie"
    );
}

fn yank_published(tag: &str, build: u64, min_build: Option<u64>) -> verify::Published {
    let version = tag.trim_start_matches('v');
    let commit = "a".repeat(40);
    verify::Published {
        release_id: Some(build),
        release: None,
        tag: tag.into(),
        build,
        version: version.into(),
        asset: manifest_out::MANIFEST_ASSET.into(),
        min_build,
        text: format!(
            "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\ncommit = \"{commit}\"\n\
             dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n{}",
            "0".repeat(64),
            min_build.map_or_else(String::new, |floor| format!("min_build = {floor}\n"))
        ),
    }
}

struct HistoricalTagGit {
    output: Mutex<Option<ledger::RunOut>>,
}

impl HistoricalTagGit {
    fn with_stdout(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            output: Mutex::new(Some(ledger::RunOut {
                status: 0,
                stdout: stdout.into(),
                stderr: Vec::new(),
            })),
        }
    }
}

impl ledger::GitRunner for HistoricalTagGit {
    fn git(&self, args: &[&str]) -> ledger::Result<ledger::RunOut> {
        assert_eq!(&args[..3], &["ls-remote", "--tags", "origin"]);
        self.output
            .lock()
            .expect("historical tag output lock")
            .take()
            .ok_or_else(|| ledger::Error::new("unexpected repeated tag query"))
    }
}

#[test]
fn historical_tag_binding_accepts_annotated_and_lightweight_exact_refs() {
    let tag_object = "1".repeat(40);
    let annotated_commit = "a".repeat(40);
    let lightweight_commit = "b".repeat(40);
    let output = format!(
        "{tag_object}\trefs/tags/v0.25.0\n{annotated_commit}\trefs/tags/v0.25.0^{{}}\n\
         {lightweight_commit}\trefs/tags/v0.24.0\n"
    );
    let git = HistoricalTagGit::with_stdout(output.into_bytes());
    publish::assert_remote_historical_tag_commits(
        &git,
        &[
            ("v0.25.0", annotated_commit.as_str()),
            ("v0.24.0", lightweight_commit.as_str()),
        ],
    )
    .unwrap();
}

#[test]
fn historical_tag_binding_rejects_missing_wrong_and_malformed_refs() {
    let commit = "a".repeat(40);
    let wrong = "b".repeat(40);
    let missing = HistoricalTagGit::with_stdout(Vec::new());
    assert!(
        publish::assert_remote_historical_tag_commits(&missing, &[("v0.25.0", commit.as_str())])
            .is_err()
    );

    let wrong_git =
        HistoricalTagGit::with_stdout(format!("{wrong}\trefs/tags/v0.25.0\n").into_bytes());
    assert!(
        publish::assert_remote_historical_tag_commits(&wrong_git, &[("v0.25.0", commit.as_str())])
            .is_err()
    );

    let malformed_git = HistoricalTagGit::with_stdout(
        format!("{commit}\trefs/tags/v0.25.0\n{commit}\trefs/tags/v0.25.0^{{}}\n").into_bytes(),
    );
    assert!(
        publish::assert_remote_historical_tag_commits(
            &malformed_git,
            &[("v0.25.0", commit.as_str())]
        )
        .is_err()
    );
}

#[test]
fn published_snapshot_preserves_symbolic_target_but_claim_capability_does_not() {
    let manifest_commit = "a".repeat(40);
    let historical = publish::ReleaseObjectIdentity {
        id: 349_821_802,
        tag: "v0.25.0".into(),
        draft: false,
        target_commitish: "main".into(),
    };
    publish::validate_release_object_snapshot(Some(&historical), &historical).unwrap();

    // Negative control for the old false invariant: a valid historical
    // snapshot is not a current-protocol claim-SHA capability.
    assert!(
        publish::validate_release_object_capability(
            Some(&historical),
            historical.id,
            &historical.tag,
            &manifest_commit,
            false,
        )
        .is_err()
    );
    let mut drifted = historical.clone();
    drifted.target_commitish = "Main".into();
    assert!(
        publish::validate_release_object_snapshot(Some(&drifted), &historical).is_err(),
        "symbolic git targets are case-sensitive mutation capabilities"
    );

    let current = publish::ReleaseObjectIdentity {
        id: 55,
        tag: "v0.55.0".into(),
        draft: false,
        target_commitish: manifest_commit.to_ascii_uppercase(),
    };
    publish::validate_release_object_capability(
        Some(&current),
        current.id,
        &current.tag,
        &manifest_commit,
        false,
    )
    .unwrap();

    let mut scratch = yank_published("v0.25.0", historical.id, None);
    scratch.release_id = Some(historical.id);
    scratch.release = Some(historical.clone());
    assert!(
        verify::validate_unbound_published_target(&scratch).is_err(),
        "scratch/rehearsal scans cannot borrow an unrelated origin tag binding"
    );
    scratch.release.as_mut().unwrap().target_commitish = manifest_commit;
    verify::validate_unbound_published_target(&scratch).unwrap();
}

/// A MIRRORED channel head is valid with a default-branch target, and the private
/// claim-SHA capability check must keep rejecting it.
///
/// This is the invariant `step_mirror` violated on v0.6.0 and v0.7.0. Both cuts
/// created the public draft, uploaded the assets and flipped it live — correctly —
/// and then `prove_mirror_channel_head` ran the PRIVATE scan over the channel and
/// refused, because a mirrored release is anchored at the channel's default branch
/// (`create_mirror_draft` sends no `target_commitish`: the claim commit does not
/// exist in that repository). The releases were right; the cut wedged with the
/// lease still held and needed a hand-edited journal to finish.
///
/// The first assertion is the negative control. It must stay: it is what stops
/// someone "fixing" the channel case by relaxing the private capability check,
/// which every scratch/rehearsal path depends on.
#[test]
fn a_mirrored_channel_head_is_valid_with_a_default_branch_target() {
    let claim = "d".repeat(40);
    let mirrored = publish::ReleaseObjectIdentity {
        id: 360_201_027,
        tag: "v0.7.0".into(),
        draft: false,
        target_commitish: "main".into(),
    };

    // NEGATIVE CONTROL — the private invariant still refuses this object.
    let mut row = yank_published("v0.7.0", 1_785_125_098, None);
    row.release_id = Some(mirrored.id);
    row.release = Some(mirrored.clone());
    row.tag = "v0.7.0".into();
    assert!(
        verify::validate_unbound_published_target(&row).is_err(),
        "the private claim-SHA capability must keep rejecting a default-branch \
         target — scratch and rehearsal scans depend on it"
    );

    // ...and the object itself is a perfectly good release: same id, same tag,
    // published. Only the target differs, and on a channel it must.
    publish::validate_release_object_snapshot(Some(&mirrored), &mirrored).unwrap();
    assert!(
        publish::validate_release_object_capability(
            Some(&mirrored),
            mirrored.id,
            &mirrored.tag,
            &claim,
            false,
        )
        .is_err(),
        "a claim SHA can never equal a channel's default branch name"
    );
}

#[test]
fn release_identity_parsers_do_not_lowercase_symbolic_targets() {
    let response =
        br#"{"id":55,"tag_name":"v0.55.0","draft":false,"target_commitish":"Feature/Case"}"#;
    assert_eq!(
        publish::parse_release_object_response(response)
            .unwrap()
            .target_commitish,
        "Feature/Case"
    );
    let rows = "55\tv0.55.0\tfalse\tFeature/Case\n";
    assert_eq!(
        publish::parse_release_object_identity_rows(rows).unwrap()[0].target_commitish,
        "Feature/Case"
    );
}

#[test]
fn release_identity_queries_match_collection_and_object_json_shapes() {
    let listing = publish::release_identity_jq(true);
    let exact = publish::release_identity_jq(false);
    assert!(listing.starts_with(".[] | ["));
    assert!(exact.starts_with("[.id,"));
    assert!(!exact.starts_with(".[]"));
    assert_eq!(listing.trim_start_matches(".[] | "), exact);
}

#[test]
fn yank_successor_first_requires_newer_order_build_and_checked_floor() {
    let bad = yank_published("v0.54.0", 54, None);
    let successor = yank_published("v0.55.0", 55, Some(55));
    assert!(verify::yank_successor_covers(&bad, &successor).unwrap());
    assert!(
        !verify::yank_successor_covers(&bad, &yank_published("v0.55.0", 55, Some(54))).unwrap()
    );
    assert!(
        !verify::yank_successor_covers(&bad, &yank_published("v0.53.0", 56, Some(55))).unwrap()
    );
    assert!(verify::yank_successor_covers(&bad, &yank_published("v0.55.0", 54, Some(55))).is_err());

    let overflow = yank_published("v0.54.0", u64::MAX, None);
    assert!(verify::yank_successor_covers(&overflow, &successor).is_err());
    let mut mismatched = successor.clone();
    mismatched.version = "0.56.0".into();
    assert!(verify::yank_successor_covers(&bad, &mismatched).is_err());

    // A retired two-component release is inert archive history that no client
    // selects. It is not orderable against the current scheme, so it is
    // refused at BOTH ends — never ordered, never a licence to delete.
    let retired = yank_published("v0.61", 61, Some(61));
    for (bad_end, successor_end) in [(&retired, &successor), (&bad, &retired)] {
        let err = verify::yank_successor_covers(bad_end, successor_end)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("yank needs two current-scheme vMAJOR.MINOR.PATCH releases")
                && err.contains("retired two-component release"),
            "{} → {}: {err}",
            bad_end.tag,
            successor_end.tag
        );
    }
}

fn appcast(version: &str, build: u64) -> Vec<u8> {
    appcast_with_floor(version, build, None)
}

fn appcast_with_floor(version: &str, build: u64, min_build: Option<u64>) -> Vec<u8> {
    let floor = min_build.map_or_else(String::new, |floor| format!("min_build = {floor}\n"));
    format!(
        "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\n\
         dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n{floor}",
        "0".repeat(64),
    )
    .into_bytes()
}

fn release_metadata_row(
    tag: &str,
    draft: bool,
    exact_count: usize,
    archive_count: usize,
) -> String {
    format!("{tag}\t{draft}\t{exact_count}\t{archive_count}")
}

/// GitHub documents no List Releases row ordering. Numeric authority and the
/// one-manifest latency bound therefore hold for every permutation; historical
/// 503s are irrelevant because their manifests are never requested.
#[test]
fn client_arbitration_is_permutation_invariant_and_skips_older_503() {
    let rows = [
        release_metadata_row("v0.9.0", false, 1, 0),
        release_metadata_row("v0.10.0", false, 1, 0),
        release_metadata_row("v0.8.0", false, 1, 0),
    ];
    for order in [
        [0usize, 1usize, 2usize],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let listing = order.map(|index| rows[index].as_str()).join("\n");
        let mut fetched = Vec::new();
        let (release_count, scanned) =
            verify::scan_release_page(&listing, true, |_, tag, asset| {
                fetched.push((tag.to_string(), asset.to_string()));
                match tag {
                    "v0.10.0" => Ok(appcast_with_floor("0.10.0", 10, Some(9))),
                    "v0.9.0" | "v0.8.0" => {
                        Err(ledger::Error::new(format!("{tag} appcast: HTTP 503")))
                    }
                    other => panic!("unexpected tag {other}"),
                }
            })
            .expect("canonical maximum must complete without historical fetches");

        assert_eq!(release_count, 3);
        assert_eq!(
            fetched,
            [("v0.10.0".to_string(), manifest_out::MANIFEST_ASSET.into())]
        );
        assert_eq!(scanned.len(), 1);
        assert_eq!((scanned[0].tag.as_str(), scanned[0].build), ("v0.10.0", 10));
        assert_eq!(scanned[0].min_build, Some(9));
    }
}

/// The repository's thirteen real pre-canonical exact heads are ordinary
/// three-component candidates under the single-version scheme — they simply
/// order BELOW the current head, so only v0.54.0's manifest may be fetched.
#[test]
fn client_arbitration_tolerates_real_lower_numeric_legacy_heads() {
    let legacy_tags = [
        "v0.21.2607041853",
        "v0.20.2607041751",
        "v0.19.2607040807",
        "v0.18.2607040011",
        "v0.17.2607032327",
        "v0.15.2607031838",
        "v0.15.2607021856",
        "v0.5.14",
        "v0.5.13",
        "v0.5.12",
        "v0.5.11",
        "v0.5.10",
        "v0.5.9",
    ];
    let mut rows: Vec<String> = legacy_tags
        .iter()
        .rev()
        .map(|tag| release_metadata_row(tag, false, 1, 0))
        .collect();
    rows.insert(6, release_metadata_row("v0.54.0", false, 1, 0));
    let listing = rows.join("\n");
    let mut fetched = Vec::new();
    let (_, scanned) = verify::scan_release_page(&listing, true, |_, tag, asset| {
        fetched.push((tag.to_string(), asset.to_string()));
        match tag {
            "v0.54.0" => Ok(appcast("0.54.0", 540)),
            legacy => panic!("legacy authority was fetched: {legacy}"),
        }
    })
    .expect("lower numeric legacy tags are provably historical");

    assert_eq!(
        fetched,
        [("v0.54.0".to_string(), manifest_out::MANIFEST_ASSET.into())]
    );
    assert_eq!(
        (scanned[0].tag.as_str(), scanned[0].build),
        ("v0.54.0", 540)
    );
}

/// Garbage in the tag namespace fails closed rather than silently narrowing
/// the candidate set: a tag that is neither canonical `vMAJOR.MINOR.PATCH`
/// nor a retired two-component release aborts the whole scan before any
/// download. Leading-zero spellings are refused too, so two tags can never
/// share one numeric order.
#[test]
fn client_arbitration_refuses_unorderable_tags() {
    for rejected in [
        "release-old",
        "v0.54.0.1",
        "v01.54.0",
        "v0.54.00",
        "v0.54.",
        "v0",
        "0.54.0",
    ] {
        let listing = [
            release_metadata_row("v0.54.0", false, 1, 0),
            release_metadata_row(rejected, false, 1, 0),
        ]
        .join("\n");
        let mut fetched = false;
        let err = verify::scan_release_page(&listing, true, |_, _, _| {
            fetched = true;
            Ok(appcast("0.54.0", 540))
        })
        .unwrap_err()
        .to_string();
        assert!(
            !fetched,
            "{rejected:?}: metadata refusal must precede every download"
        );
        assert!(
            err.contains("is not numeric dotted vN.N.N"),
            "{rejected:?} → {err}"
        );
    }
}

/// The burned bridge, executable: retired two-component releases stay
/// published in the archive and are neither candidates nor errors. A live
/// v0.61 orders above v0.2.0 under the OLD two-field comparison, so without
/// the skip the very first post-cut-over check would stall on it.
#[test]
fn client_arbitration_skips_retired_two_component_releases() {
    let listing = [
        release_metadata_row("v0.61", false, 1, 0),
        release_metadata_row("v0.2.0", false, 1, 0),
        release_metadata_row("v0.25", false, 1, 0),
    ]
    .join("\n");
    let mut fetched = Vec::new();
    let (release_count, scanned) = verify::scan_release_page(&listing, true, |_, tag, asset| {
        fetched.push((tag.to_string(), asset.to_string()));
        match tag {
            "v0.2.0" => Ok(appcast("0.2.0", 1_790_000_000)),
            retired => panic!("a retired two-component release was fetched: {retired}"),
        }
    })
    .expect("retired releases are archive history, never an error");

    assert_eq!(
        release_count, 3,
        "every row is still counted for pagination"
    );
    assert_eq!(
        fetched,
        [("v0.2.0".to_string(), manifest_out::MANIFEST_ASSET.into())]
    );
    assert_eq!(
        (scanned[0].tag.as_str(), scanned[0].build),
        ("v0.2.0", 1_790_000_000)
    );

    // With no current-scheme candidate at all, a page of retired releases
    // selects NOTHING — it must never fall back to archive history.
    let retired_only = [
        release_metadata_row("v0.61", false, 1, 0),
        release_metadata_row("v0.25", false, 1, 0),
    ]
    .join("\n");
    let (_, none) = verify::scan_release_page(&retired_only, true, |_, tag, _| {
        panic!("retired-only page must fetch nothing, got {tag}")
    })
    .expect("a retired-only page is empty, not malformed");
    assert!(none.is_empty());
}

#[test]
fn client_arbitration_rejects_malformed_and_duplicate_metadata_before_fetch() {
    let fixtures = [
        release_metadata_row("v0.54.0", false, 2, 0),
        [
            release_metadata_row("v0.54.0", false, 1, 0),
            release_metadata_row("v0.54.0", false, 1, 0),
        ]
        .join("\n"),
        "v0.54.0\tfalse\tnot-a-count\t0".to_string(),
        "v0.54.0\tfalse\t1".to_string(),
    ];
    for listing in fixtures {
        let mut fetched = false;
        let err = verify::scan_release_page(&listing, true, |_, _, _| {
            fetched = true;
            Ok(appcast("0.54.0", 540))
        })
        .unwrap_err()
        .to_string();
        assert!(!fetched, "metadata refusal must precede every download");
        assert!(
            err.contains("duplicate") || err.contains("malformed release metadata"),
            "{err}"
        );
    }
}

/// An authoritative fetch or validation failure is terminal: an older valid
/// manifest must never become a fallback authority.
#[test]
fn authoritative_fetch_and_version_mismatch_never_fall_back() {
    let listing = [
        release_metadata_row("v0.9.0", false, 1, 0),
        release_metadata_row("v0.10.0", false, 1, 0),
    ]
    .join("\n");

    let mut fetched = Vec::new();
    let err = verify::scan_release_page(&listing, true, |_, tag, _| {
        fetched.push(tag.to_string());
        match tag {
            "v0.10.0" => Err(ledger::Error::new("authoritative appcast: HTTP 503")),
            older => panic!("fallback fetched after authoritative failure: {older}"),
        }
    })
    .unwrap_err()
    .to_string();
    assert_eq!(fetched, ["v0.10.0"]);
    assert!(err.contains("HTTP 503"), "{err}");

    fetched.clear();
    let err = verify::scan_release_page(&listing, true, |_, tag, _| {
        fetched.push(tag.to_string());
        match tag {
            "v0.10.0" => Ok(appcast("0.9.0", 10)),
            older => panic!("fallback fetched after version rejection: {older}"),
        }
    })
    .unwrap_err()
    .to_string();
    assert_eq!(fetched, ["v0.10.0"]);
    assert!(
        err.contains("manifest version") && err.contains("0.10.0"),
        "{err}"
    );
}

/// Archived names are normal historical metadata and invisible to the client
/// lane; only the exhaustive operator lane may fetch them.
#[test]
fn client_replay_ignores_archived_only_history() {
    let mut fetched = false;
    let (_, scanned) = verify::scan_release_page(
        &release_metadata_row("v0.41.0", false, 0, 1),
        true,
        |_, _, _| {
            fetched = true;
            Ok(appcast("0.41.0", 410))
        },
    )
    .unwrap();
    assert!(!fetched);
    assert!(scanned.is_empty());
}

/// Negative control for the exact retired behavior: disabling the early stop
/// reaches the injected old-release 503 and fails. If the positive fixture did
/// not distinguish the two policies, this assertion could not pass.
#[test]
fn no_stop_negative_control_reproduces_old_appcast_503() {
    let listing = [
        release_metadata_row("v0.54.0", false, 1, 0),
        release_metadata_row("v0.41.0", false, 0, 1),
    ]
    .join("\n");
    let mut fetched = Vec::new();
    let err = verify::scan_release_page(&listing, false, |_, tag, asset| {
        fetched.push((tag.to_string(), asset.to_string()));
        match tag {
            "v0.54.0" => Ok(appcast("0.54.0", 540)),
            "v0.41.0" => Err(ledger::Error::new("v0.41.0 appcast: HTTP 503")),
            other => panic!("unexpected tag {other}"),
        }
    })
    .expect_err("the no-stop mutant must reproduce the obsolete-download failure");

    assert_eq!(
        fetched,
        [
            ("v0.54.0".to_string(), manifest_out::MANIFEST_ASSET.into()),
            (
                "v0.41.0".to_string(),
                manifest_out::archived_manifest_asset("v0.41.0")
            )
        ]
    );
    assert!(err.to_string().contains("v0.41.0 appcast: HTTP 503"));
}

/// `cargo ship status` intentionally requests the exhaustive policy: drafts
/// and appcast-less releases remain invisible, while every published appcast
/// is downloaded so dangling ledger claims can be computed over the full set.
#[test]
fn exhaustive_page_preserves_all_published_appcasts() {
    let listing = [
        release_metadata_row("v0.55.0", true, 1, 0),
        release_metadata_row("v0.54.0", false, 1, 0),
        release_metadata_row("v0.53.0", false, 0, 0),
        release_metadata_row("v0.41.0", false, 0, 1),
    ]
    .join("\n");
    let mut fetched = Vec::new();
    let (page_len, scanned) = verify::scan_release_page(&listing, false, |_, tag, asset| {
        fetched.push((tag.to_string(), asset.to_string()));
        match tag {
            "v0.54.0" => Ok(appcast("0.54.0", 540)),
            "v0.41.0" => Ok(appcast("0.41.0", 410)),
            other => panic!("draft/appcast-less tag was fetched: {other}"),
        }
    })
    .expect("exhaustive status scan");

    assert_eq!(page_len, 4);
    assert_eq!(
        fetched,
        [
            ("v0.54.0".to_string(), manifest_out::MANIFEST_ASSET.into()),
            (
                "v0.41.0".to_string(),
                manifest_out::archived_manifest_asset("v0.41.0")
            )
        ]
    );
    assert_eq!(
        scanned
            .iter()
            .map(|published| (published.tag.as_str(), published.build))
            .collect::<Vec<_>>(),
        [("v0.54.0", 540), ("v0.41.0", 410)]
    );
    assert_eq!(
        verify::select_newest(&scanned).map(|published| published.tag.as_str()),
        Some("v0.54.0")
    );
    assert_eq!(
        scanned[1].asset,
        manifest_out::archived_manifest_asset("v0.41.0"),
        "exhaustive status/yank history must retain renamed manifests"
    );
}

/// The production jq carries both exact and archive counts, so neither lane can
/// collapse duplicate names to an arbitrary `[0]` result.
#[test]
fn exhaustive_history_rejects_duplicate_exact_or_archive_names() {
    for listing in [
        release_metadata_row("v0.54.0", false, 2, 0),
        release_metadata_row("v0.41.0", false, 0, 2),
        release_metadata_row("v0.54.0", false, 1, 1),
    ] {
        let mut fetched = false;
        let err = verify::scan_release_page(&listing, false, |_, _, _| {
            fetched = true;
            Ok(appcast("0.54.0", 540))
        })
        .unwrap_err()
        .to_string();
        assert!(!fetched);
        assert!(
            err.contains("duplicate assets") || err.contains("both exact"),
            "{err}"
        );
    }
}

// ---------------------------------------------------------------------------
// the hand-rolled CLI (spec §5 surface)
// ---------------------------------------------------------------------------

fn parse(args: &[&str]) -> Result<cli::Cmd, String> {
    cli::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

#[test]
fn cli_parses_the_whole_spec_5_surface() {
    let successor = cli::USAGE
        .find("successor FIRST")
        .expect("help documents successor-first yank");
    let cleanup = cli::USAGE
        .find("only then remove")
        .expect("help documents post-proof cleanup");
    assert!(
        successor < cleanup,
        "help must never suggest delete-first yank"
    );
    assert_eq!(parse(&[]).unwrap(), cli::Cmd::Help);
    assert_eq!(parse(&["--help"]).unwrap(), cli::Cmd::Help);
    assert_eq!(parse(&["status"]).unwrap(), cli::Cmd::Status);
    assert_eq!(
        parse(&["provision", "--id", "m2"]).unwrap(),
        cli::Cmd::Provision {
            id: "m2".into(),
            check: false,
            cert_dir: None,
        }
    );
    assert_eq!(
        parse(&["provision", "--id", "m2", "--check"]).unwrap(),
        cli::Cmd::Provision {
            id: "m2".into(),
            check: true,
            cert_dir: None,
        }
    );
    // The one folder the browser errand may use is NAMED, never assumed: without the
    // flag provision reads only ~/.aterm/apple (the 2026-09-12 TCC audit's Downloads
    // poll), and the flag takes exactly one folder.
    assert_eq!(
        parse(&[
            "provision",
            "--id",
            "m2",
            "--cert-dir",
            "/Users//x/Downloads"
        ])
        .unwrap(),
        cli::Cmd::Provision {
            id: "m2".into(),
            check: false,
            cert_dir: Some("/Users//x/Downloads".into()),
        }
    );
    assert!(
        cli::USAGE.contains("--cert-dir <folder>"),
        "help must document the one folder the errand may use"
    );
    assert!(
        cli::USAGE.contains("~/Downloads or another folder macOS guards on its own"),
        "help must say the default reads nothing outside ~/.aterm/apple"
    );
    assert!(
        cli::USAGE.contains("provision --id"),
        "help must document the one-command machine provisioning verb"
    );
    assert!(
        cli::USAGE.contains("--check"),
        "help must document the no-writes audit rehearsal"
    );
    assert!(
        cli::USAGE.contains(publish::RECOVERY_STOPPED_PROCESS_FLAG),
        "help must name the mandatory stopped-publisher acknowledgement"
    );
    assert_eq!(
        parse(&[
            "recover",
            "v0.55.0",
            &"a".repeat(40),
            publish::RECOVERY_STOPPED_PROCESS_FLAG,
        ])
        .unwrap(),
        cli::Cmd::Recover {
            version: "0.55.0".into(),
            owner: "a".repeat(40),
            release_credentials: None,
            no_draft_posted: false,
        }
    );
    // The absent-draft assertion is OPT-IN and separate from the mandatory
    // stopped-publisher one: they claim different things, and folding them would make
    // the weaker claim silently every time the stronger one is required.
    assert_eq!(
        parse(&[
            "recover",
            "v0.55.0",
            &"a".repeat(40),
            publish::RECOVERY_STOPPED_PROCESS_FLAG,
            publish::RECOVERY_NO_DRAFT_POSTED_FLAG,
        ])
        .unwrap(),
        cli::Cmd::Recover {
            version: "0.55.0".into(),
            owner: "a".repeat(40),
            release_credentials: None,
            no_draft_posted: true,
        }
    );
    assert_eq!(
        parse(&["verify"]).unwrap(),
        cli::Cmd::Verify { version: None }
    );
    assert_eq!(
        parse(&["verify", "v0.26.0"]).unwrap(),
        cli::Cmd::Verify {
            version: Some("0.26.0".into())
        },
        "the v prefix is normalized away"
    );
    assert_eq!(
        parse(&["yank", "1783918101"]).unwrap(),
        cli::Cmd::Yank {
            build: 1_783_918_101,
            opts: verify::YankOptions::default()
        }
    );
    // A yank publishes a real cut before it deletes anything, and since the paper
    // master was armed that cut refuses pre-claim unless it is told which profile
    // signs. Pinned here because the way this breaks is a parse that SUCCEEDS and
    // drops the answer.
    assert_eq!(
        parse(&[
            "yank",
            "1783918101",
            "--release-credentials",
            "/keys/m3.toml"
        ])
        .unwrap(),
        cli::Cmd::Yank {
            build: 1_783_918_101,
            opts: verify::YankOptions {
                release_credentials: Some(PathBuf::from("/keys/m3.toml")),
            }
        }
    );
    // Every OTHER cut flag stays refused: a yank fixes its own min_build and
    // publishes a real successor, so accepting one could only mean ignoring it.
    let yank_flag_error = parse(&["yank", "1783918101", "--dry-run"]).unwrap_err();
    assert!(
        yank_flag_error.contains("--release-credentials"),
        "{yank_flag_error}"
    );

    let cli::Cmd::Cut { opts, abandon, .. } = parse(&[
        "cut",
        "--dry-run",
        "--min-build",
        "42",
        "--gate",
        "--arm64-only",
    ])
    .unwrap() else {
        panic!("expected Cut");
    };
    assert!(abandon.is_none());
    assert!(opts.dry_run && opts.gate && opts.arm64_only && !opts.resume);
    assert_eq!(opts.min_build, Some(42));

    let cli::Cmd::Cut { opts, .. } =
        parse(&["cut", "--rehearse", "alabsystems/aterm-rehearsal"]).unwrap()
    else {
        panic!("expected Cut");
    };
    assert_eq!(
        opts.rehearse.as_deref(),
        Some("alabsystems/aterm-rehearsal")
    );

    let cli::Cmd::Cut { abandon, .. } = parse(&["cut", "--abandon", "v0.26.0"]).unwrap() else {
        panic!("expected Cut");
    };
    assert_eq!(abandon.as_deref(), Some("0.26.0"));
}

#[test]
fn cli_rejects_malformed_and_conflicting_invocations() {
    for (args, needle) in [
        (vec!["frobnicate"], "unknown command"),
        (vec!["cut", "--frobnicate"], "unknown cut flag"),
        (vec!["provision"], "--id"),
        (vec!["provision", "--id"], "needs a machine id"),
        (
            vec!["provision", "--id", "m2", "--cert-dir"],
            "needs a folder",
        ),
        (
            vec!["provision", "--id", "m2", "--cert-dir", ""],
            "not an empty string",
        ),
        (
            vec![
                "provision",
                "--id",
                "m2",
                "--cert-dir",
                "a",
                "--cert-dir",
                "b",
            ],
            "given twice",
        ),
        (
            vec!["provision", "--frobnicate", "m2"],
            "unknown provision flag",
        ),
        (
            vec!["provision", "--id", "m2", "--id", "m3"],
            "--id given twice",
        ),
        // The version is Cargo.toml's, and nothing on the command line
        // overrides it: the retired override is an unknown flag.
        (vec!["cut", "--set-version", "0.26.0"], "unknown cut flag"),
        // --abandon takes only the canonical three-component form; the
        // retired two-component spelling is just another malformed one.
        (vec!["cut", "--abandon", "0.26"], "MAJOR.MINOR.PATCH"),
        (vec!["cut", "--abandon", "01.26.0"], "MAJOR.MINOR.PATCH"),
        (vec!["cut", "--min-build", "abc"], "not a u64"),
        (
            vec![
                "recover",
                "v0.55.0",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            "--old-publisher-stopped",
        ),
        (vec!["cut", "--rehearse", "no-slash"], "OWNER/REPO"),
        (vec!["cut", "--rehearse"], "OWNER/REPO"),
        (vec!["cut", "--resume", "--dry-run"], "--resume"),
        (vec!["cut", "--resume", "--min-build", "7"], "--resume"),
        (vec!["cut", "--abandon", "v0.26.0", "--gate"], "--abandon"),
        (
            vec!["cut", "--dry-run", "--rehearse", "o/r"],
            "mutually exclusive",
        ),
        (vec!["yank"], "build number"),
        (vec!["yank", "abc"], "not a build number"),
        // The stray-argument refusal changed WORDING when yank grew the two
        // signing flags its successor cut needs on the armed tree: it can no
        // longer say a yank takes "exactly one" argument, because a yank now
        // legitimately takes more. Only the BUILD NUMBER is still one-of, so the
        // needle moves onto that surviving guarantee instead of being deleted —
        // dropping the row would leave the extra-positional arm of the new parse
        // loop with no test at all.
        (vec!["yank", "1", "2"], "yank takes one build number"),
        // And the value-taking flag must not swallow a trailing positional, must
        // not accept a missing value, and must not be given twice: two profiles
        // mean one of them is ignored, and the ignored one may be the one that
        // was supposed to sign the successor.
        (
            vec!["yank", "1", "--release-credentials", "/keys/m3.toml", "2"],
            "yank takes one build number",
        ),
        (vec!["yank", "1", "--release-credentials"], "needs a path"),
        (
            vec![
                "yank",
                "1",
                "--release-credentials",
                "/keys/m3.toml",
                "--release-credentials",
                "/keys/m4.toml",
            ],
            "--release-credentials given twice",
        ),
        (vec!["status", "extra"], "no arguments"),
        (vec!["recover", "v0.55.0"], "full claim SHA"),
        (
            vec!["recover", "v0.55.0", "abc", "extra"],
            "--old-publisher-stopped",
        ),
        (vec!["verify", "v0.26.0", "extra"], "at most one"),
        (vec!["verify", "not-a-version"], "MAJOR.MINOR.PATCH"),
        (vec!["verify", "v0.26"], "MAJOR.MINOR.PATCH"),
        (vec!["cut", "--abandon", "v0.26"], "MAJOR.MINOR.PATCH"),
        (
            vec![
                "recover",
                "v0.55",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            "MAJOR.MINOR.PATCH",
        ),
    ] {
        let err = parse(&args).unwrap_err();
        assert!(
            err.contains(needle),
            "{args:?} → {err:?} (wanted {needle:?})"
        );
    }
}

/// THE RETIRED PRE-ROSTER FLAG IS REFUSED BY NAME, in one sentence, on every verb that
/// used to take it — a leftover spelling in a script or a runbook must not read as an
/// unknown flag, and must never be silently accepted.
#[test]
fn the_retired_strand_flag_is_refused_in_one_sentence() {
    for args in [
        vec!["cut", cli::RETIRED_STRAND_FLAG],
        vec!["cut", "--dry-run", cli::RETIRED_STRAND_FLAG],
        vec!["yank", "1783918101", cli::RETIRED_STRAND_FLAG],
    ] {
        let err = parse(&args).unwrap_err();
        assert_eq!(err, cli::RETIRED_STRAND_REFUSAL, "{args:?}");
        assert!(
            err.contains("retired") && err.contains("drop the flag"),
            "{err}"
        );
        assert_eq!(err.matches(". ").count(), 0, "one sentence: {err}");
    }
    // Negative control: a cut without it parses, and the usage no longer teaches it.
    assert!(parse(&["cut"]).is_ok());
    assert!(
        !cli::USAGE.contains(cli::RETIRED_STRAND_FLAG),
        "{}",
        cli::USAGE
    );
}

#[test]
fn recovery_requires_and_labels_the_external_stop_precondition() {
    let owner = "a".repeat(40);
    let extra = parse(&[
        "recover",
        "v0.55.0",
        &owner,
        publish::RECOVERY_STOPPED_PROCESS_FLAG,
        "extra",
    ])
    .unwrap_err();
    // recover now ACCEPTS --release-credentials (it signs, so it needs the same
    // explicit key as a fresh cut) but nothing else: an unknown argument is still
    // refused, and the refusal names it.
    assert!(
        extra.contains("extra"),
        "the refusal must name the argument: {extra}"
    );
    assert!(
        extra.contains("--release-credentials"),
        "and must say what IS accepted: {extra}"
    );
    // The one accepted flag parses.
    parse(&[
        "recover",
        "v0.55.0",
        &owner,
        publish::RECOVERY_STOPPED_PROCESS_FLAG,
        "--release-credentials",
        "/tmp/creds.toml",
    ])
    .expect("recover accepts the credentials flag");

    // False must refuse before inspecting this deliberately nonexistent
    // repository. The boolean is an operator assertion, never a machine proof.
    let error = publish::run_recover_lost(
        &PathBuf::from("/definitely/not/an/aterm/repository"),
        "0.55.0",
        &owner,
        false,
        false,
        None,
        None,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, publish::RECOVERY_STOPPED_PROCESS_REFUSAL);
    assert!(publish::RECOVERY_STOPPED_PROCESS_BANNER.contains("OPERATOR ASSERTION"));
    assert!(publish::RECOVERY_STOPPED_PROCESS_BANNER.contains("cannot cancel"));
}

// ---------------------------------------------------------------------------
// resume's provenance gate (the fresh cut's pre-claim gate, re-run before a rebuild)
// ---------------------------------------------------------------------------

/// A resume that still has `build` to do runs the provenance gate BEFORE the pipeline,
/// and its refusal carries the gate's own words plus the step that would rebuild.
///
/// MUTATION TARGET: delete the call in `resume_cut`, or make the early return
/// unconditional — a tracked shell's `--resume` then bakes tagged artifacts and dies at
/// the proof snapshot with the build number already burned.
#[test]
fn a_resume_that_will_rebuild_runs_the_provenance_gate_first() {
    let mut j = journal();
    j.done = vec!["lock".into()];
    let called = std::cell::Cell::new(false);
    let err = publish::resume_provenance_gate(&j, || {
        called.set(true);
        Err(ledger::Error::new(
            "trustc: /s/bin/trustc carries com.apple.provenance",
        ))
    })
    .expect_err("a tagged toolchain must not be rebuilt with");
    assert!(called.get(), "the gate was consulted");
    let msg = err.to_string();
    assert!(msg.contains("would rebuild"), "{msg}");
    assert!(msg.contains("\"build\""), "the step named: {msg}");
    assert!(
        msg.contains("trustc: /s/bin/trustc carries com.apple.provenance"),
        "the gate's own words: {msg}"
    );
    // A gate that passes lets the rebuild through.
    assert!(publish::resume_provenance_gate(&j, || Ok(())).is_ok());
    // And a journal that has not even locked yet is still "will rebuild".
    let fresh = journal();
    assert!(publish::resume_provenance_gate(&fresh, || Err(ledger::Error::new("x"))).is_err());
}

/// A resume past `build` never consults the gate: nothing remaining compiles, so a
/// tagged compiler cannot reach a file, and a cut one upload from finished must stay
/// finishable — even from a tracked shell, even with a tagged toolchain installed.
///
/// MUTATION TARGET: drop the `is_done("build")` early return — the closure below then
/// runs, returns its refusal, and this test goes red.
#[test]
fn a_resume_past_the_build_never_consults_the_provenance_gate() {
    for done in [
        vec!["lock", "build"],
        vec!["lock", "build", "selfcheck", "draft"],
        vec![
            "lock",
            "build",
            "selfcheck",
            "draft",
            "upload",
            "preflip",
            "tag",
            "flip",
            "archive",
            "verify",
            "mirror",
            "unlock",
        ],
    ] {
        let mut j = journal();
        j.done = done.iter().map(|s| (*s).to_string()).collect();
        let called = std::cell::Cell::new(false);
        publish::resume_provenance_gate(&j, || {
            called.set(true);
            Err(ledger::Error::new(
                "this cutter PROCESS is provenance-tracked",
            ))
        })
        .unwrap_or_else(|e| panic!("a resume past build must not be blocked ({done:?}): {e}"));
        assert!(
            !called.get(),
            "the gate must not even be consulted ({done:?})"
        );
    }
}

/// The production gate a rebuilding resume runs IS the fresh cut's gate over the same
/// toolchain: on this machine the two answer identically, pass or refusal, word for word.
/// Both run READ-ONLY here ([`keep_the_installed_toolchain`]): this reads the machine's
/// installed toolchain, and a test run must not rewrite it.
#[cfg(target_os = "macos")]
#[test]
fn the_resume_gate_is_the_fresh_cuts_gate_on_this_machine() {
    let fresh = gates::trust_stage2_bin()
        .and_then(|bin| {
            gates::provenance_gate_with(&bin.join("trustc"), keep_the_installed_toolchain)
        })
        .map_err(|e| e.to_string());
    let resumed = publish::toolchain_provenance_gate_with(keep_the_installed_toolchain)
        .map_err(|e| e.to_string());
    assert_eq!(resumed, fresh);
    let mut j = journal();
    j.done = vec!["lock".into()];
    let through_the_rule = publish::resume_provenance_gate(&j, || {
        publish::toolchain_provenance_gate_with(keep_the_installed_toolchain)
    })
    .map_err(|e| e.to_string());
    match (&fresh, &through_the_rule) {
        (Ok(()), Ok(())) => {}
        (Err(f), Err(r)) => assert!(r.contains(f.as_str()), "{r}\n--- vs ---\n{f}"),
        other => panic!("the rule must agree with the gate: {other:?}"),
    }
}

/// A heal that changes nothing: the provenance gate's heal step, for a test that runs
/// the gate over the machine's INSTALLED toolchain. The real heal is exercised on a
/// scratch toolchain in the gates' own tests.
#[cfg(target_os = "macos")]
fn keep_the_installed_toolchain(
    _roots: &[std::path::PathBuf],
    _scratch: &std::path::Path,
) -> atpkg::provenance::HealOutcome {
    atpkg::provenance::HealOutcome::Clean
}
