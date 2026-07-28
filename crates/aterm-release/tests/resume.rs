// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resume/recut logic proofs (release spec §5): the journal's re-entry
//! contract and the remote-derived cut-mode decision table, as PURE logic
//! over fake remote state — no network, no git. Plus the pure publish
//! helpers the pipeline hangs off (version bump, cask re-pin, monotonic
//! gate, channel-floor carry-forward, exhaustive status selection) and the
//! hand-rolled CLI parse table.

// The release crate is a binary on purpose (the spec's §9 file plan has no
// lib.rs), so the integration tests compile the modules under test directly.
// publish/verify pull in every pipeline stage through `crate::`, hence the
// full mount list.
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
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
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
/// version derived from `[workspace.package] version` (DEV → 0), so the
/// default cut NEVER invents a successor. "workspace-derived version already
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

    // (workspace-derived version, section?, published?, --set-version) → want
    let table: &[(&str, bool, bool, Option<&str>, CutMode)] = &[
        // Steady state: Cargo.toml says 0.3.1, so the cut is v0.3.0 — the
        // operator's bump is the ONLY thing that advances the version.
        ("0.3.0", false, false, None, fresh("0.3.0")),
        // Explicit override on a clean tree.
        ("0.3.0", false, false, Some("1.0.0"), fresh("1.0.0")),
        // THE wedge signature: roll+claim landed, nothing published ⇒ recut.
        ("0.2.0", true, false, None, recut("0.2.0")),
        // Same wedge, version named explicitly ⇒ still a recut.
        ("0.2.0", true, false, Some("0.2.0"), recut("0.2.0")),
        // Wedged 0.2.0 exists but the operator wants a different version:
        // their call — the tag/cut-elsewhere gates still stand.
        ("0.2.0", true, false, Some("0.3.0"), fresh("0.3.0")),
        // Bumped but never rolled (no section): fresh cut of the named
        // version — the roll happens inside the claim.
        ("0.2.0", false, false, Some("0.2.0"), fresh("0.2.0")),
        // Negative control for the RETIRED ledger-tail bump: the old default
        // path answered fresh("0.10.0") here. There is no arithmetic left.
        ("0.9.0", false, false, None, fresh("0.9.0")),
        ("1.99.0", false, false, None, fresh("1.99.0")),
    ];
    for (short, section, published, set, want) in table {
        let got = verify::derive_cut_mode(&state(short, *section, *published), *set)
            .unwrap_or_else(|e| panic!("({short}, {section}, {published}, {set:?}) errored: {e}"));
        assert_eq!(&got, want, "({short}, {section}, {published}, {set:?})");
    }
}

/// Cutting twice without bumping `[workspace.package] version` is refused —
/// by name AND on the plain default path, which is the shape the operator
/// actually hits. The message must name the Cargo.toml bump (with the exact
/// next version) and keep the yank escape hatch.
#[test]
fn recutting_a_published_version_is_refused() {
    for set in [Some("0.2.0"), None] {
        let err = verify::derive_cut_mode(&state("0.2.0", true, true), set)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("v0.2.0 is already published"),
            "{set:?}: {err}"
        );
        assert!(
            err.contains("bump [workspace.package] version in Cargo.toml"),
            "{set:?}: {err}"
        );
        assert!(err.contains("the next release is v0.3.0"), "{set:?}: {err}");
        assert!(err.contains("cargo ship yank <build>"), "{set:?}: {err}");
    }

    // A published version with no rolled section is the same refusal: the
    // guard keys on "published", never on the changelog.
    assert!(verify::derive_cut_mode(&state("0.2.0", false, true), None).is_err());
}

// ---------------------------------------------------------------------------
// journal-based re-entry
// ---------------------------------------------------------------------------

fn journal() -> Journal {
    Journal {
        format: publish::JOURNAL_FORMAT,
        version: "0.26.0".into(),
        build_number: 1_783_918_101,
        commit: "aed5a06caed5a06caed5a06caed5a06caed5a06c".into(),
        min_build: None,
        arm64_only: false,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
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
            "cask",
            "verify",
            // The public-channel mirror runs AFTER the private release is
            // fully verified and BEFORE the lease is released, so a mirror
            // failure is loud and resumable rather than a silently
            // private-only release the fleet can never see.
            "mirror",
            "unlock"
        ]
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
        "a crash after visibility resumes into channel convergence before cask/verify"
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

#[test]
fn completed_legacy_journal_stays_complete_instead_of_reclassifying_archive() {
    let dir = tmpdir("journal-legacy-complete");
    let path = dir.join("cut-state.toml");
    std::fs::write(
        &path,
        format!(
            "version = \"0.52.0\"\nbuild_number = 520\ncommit = \"{}\"\n\
             done = [\"build\", \"selfcheck\", \"draft\", \"upload\", \"preflip\", \
             \"tag\", \"flip\", \"cask\", \"verify\"]\n",
            "a".repeat(40)
        ),
    )
    .unwrap();

    let legacy = Journal::load(&path).unwrap().unwrap();
    assert_eq!(legacy.format, 1, "missing format is the legacy protocol");
    assert!(
        !legacy.manifest_signed,
        "legacy signed state is unknown/false"
    );
    assert_eq!(
        legacy.first_incomplete(),
        None,
        "a completed v1 journal must remain clearable, never enter v5 recovery"
    );
    legacy.ensure_resumable().unwrap();
}

#[test]
fn unfinished_legacy_journal_fails_closed_before_remote_resume() {
    let dir = tmpdir("journal-legacy-incomplete");
    let path = dir.join("cut-state.toml");
    std::fs::write(
        &path,
        format!(
            "version = \"0.52.0\"\nbuild_number = 520\ncommit = \"{}\"\n\
             done = [\"build\", \"selfcheck\", \"draft\"]\n",
            "b".repeat(40)
        ),
    )
    .unwrap();

    let legacy = Journal::load(&path).unwrap().unwrap();
    assert_eq!(legacy.first_incomplete(), Some("upload"));
    let error = legacy.ensure_resumable().unwrap_err().to_string();
    assert!(error.contains("format 1"), "{error}");
    assert!(error.contains("cannot be resumed safely"), "{error}");
    assert!(error.contains("cargo ship recover v0.52.0"), "{error}");
    assert!(error.contains(&"b".repeat(40)), "{error}");
}

#[test]
fn completed_v4_loads_but_unfinished_v4_fails_closed() {
    let dir = tmpdir("journal-v4-policy");
    let path = dir.join("cut-state.toml");
    let completed = format!(
        "format = 4\nversion = \"0.54.0\"\nbuild_number = 540\ncommit = \"{}\"\n\
         release_id = 54\ndone = [{}]\n",
        "c".repeat(40),
        STEPS
            .iter()
            .map(|step| format!("\"{step}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    std::fs::write(&path, completed).unwrap();
    let loaded = Journal::load(&path).unwrap().unwrap();
    assert_eq!(loaded.format, 4);
    assert_eq!(loaded.first_incomplete(), None);
    loaded.ensure_resumable().unwrap();

    std::fs::write(
        &path,
        format!(
            "format = 4\nversion = \"0.54.0\"\nbuild_number = 540\ncommit = \"{}\"\n\
             release_id = 54\ndone = [\"lock\", \"build\", \"selfcheck\", \"draft\"]\n",
            "c".repeat(40)
        ),
    )
    .unwrap();
    let unfinished = Journal::load(&path).unwrap().unwrap();
    let error = unfinished.ensure_resumable().unwrap_err().to_string();
    assert!(error.contains("format 4"), "{error}");
    assert!(error.contains("cannot be resumed safely"), "{error}");
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

    // Negative control for an older or hand-edited journal: load validates the
    // same invariant instead of trusting a syntactically valid TOML record.
    std::fs::write(
        &path,
        format!(
            "version = \"0.55.0\"\nbuild_number = 550\ncommit = \"{}\"\nmin_build = 551\n",
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

    // The workspace MAJOR.MINOR.0 passes through byte-exactly — the patch
    // component is only reset by `release_version_from_workspace`.
    let dev = "[workspace.package]\nversion = \"0.2.1\"\n";
    assert_eq!(publish::workspace_version(dev).unwrap(), "0.2.1");

    // THE cut-over rule: a release is the workspace version with DEV → 0.
    assert_eq!(
        publish::release_version_from_workspace("0.2.1").unwrap(),
        "0.2.0"
    );
    assert_eq!(
        publish::release_version_from_workspace("0.2.0").unwrap(),
        "0.2.0",
        "a DEV-0 workspace version is already its own release version"
    );
    assert_eq!(
        publish::release_version_from_workspace("1.10.7").unwrap(),
        "1.10.0"
    );
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

    // The REAL workspace manifest parses, is canonical, and its release
    // version is the same number with DEV reset.
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
    let mut parts = v.split('.');
    assert_eq!(
        release,
        format!("{}.{}.0", parts.next().unwrap(), parts.next().unwrap()),
        "the release version is the workspace version with DEV reset to 0"
    );
}

/// A cut rolls the changelog and NOTHING else. The workspace
/// `MAJOR.MINOR.0` version is now the single source of the release version,
/// so the cutter reading it must never also rewrite it: bumping Cargo.toml
/// stays the operator's deliberate act, and Cargo.lock is byte-untouched.
#[test]
fn release_file_regeneration_preserves_source_version_and_lock() {
    let root = tmpdir("regen-preserves-source");
    let cargo = "[workspace.package]\nversion = \"0.59.1\"\n";
    let lock = "[[package]]\nname = \"aterm\"\nversion = \"0.59.1\"\n";
    let changelog = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n- Left the workspace version to the operator.\n\n## [0.58.0] - 2026-07-22\n\n- Prior release.\n";
    std::fs::write(root.join("Cargo.toml"), cargo).unwrap();
    std::fs::write(root.join("Cargo.lock"), lock).unwrap();
    std::fs::write(root.join(changelog::CHANGELOG_FILE), changelog).unwrap();

    // The version being cut is the workspace 0.59.1 with DEV reset.
    let cut = publish::release_version_from_workspace(
        &publish::workspace_version(cargo).expect("fixture manifest parses"),
    )
    .unwrap();
    assert_eq!(cut, "0.59.0");
    let paths = publish::regen_release_files(&root, &cut, "2026-07-23").unwrap();

    assert_eq!(paths, vec![changelog::CHANGELOG_FILE.to_string()]);
    assert_eq!(
        std::fs::read_to_string(root.join("Cargo.toml")).unwrap(),
        cargo,
        "a cut never rewrites the workspace version it derived from"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("Cargo.lock")).unwrap(),
        lock
    );
    let rolled = std::fs::read_to_string(root.join(changelog::CHANGELOG_FILE)).unwrap();
    assert_eq!(
        changelog::rolled_body(&rolled, &cut).unwrap(),
        "### Fixed\n- Left the workspace version to the operator."
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
    assert!(publish::validate_release_asset_download_size(536_870_912).is_ok());
    assert!(publish::validate_release_asset_download_size(0).is_err());
    assert!(publish::validate_release_asset_download_size(536_870_913).is_err());
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

/// Cask re-pin (spec §7 step 6) against the REAL committed cask: exactly the
/// version and sha256 stanzas change; the `#{version}`-templated url line and
/// everything else are byte-untouched.
#[test]
fn cask_repin_rewrites_exactly_two_lines_of_the_real_cask() {
    let real = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packaging/homebrew/aterm.rb"
    ))
    .expect("read the real cask");
    let sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let pinned = publish::repin_cask_text(&real, "0.26.0", sha).expect("re-pin");

    let old: Vec<&str> = real.lines().collect();
    let new: Vec<&str> = pinned.lines().collect();
    assert_eq!(old.len(), new.len(), "re-pin must not add/remove lines");
    let diff: Vec<usize> = (0..old.len()).filter(|&i| old[i] != new[i]).collect();
    assert_eq!(
        diff.len(),
        2,
        "exactly version + sha256 lines change: {diff:?}"
    );
    assert!(new[diff[0]].trim_start().starts_with("version \"0.26.0\""));
    assert!(
        new[diff[1]]
            .trim_start()
            .starts_with(&format!("sha256 \"{sha}\""))
    );
    assert!(
        pinned.contains("url \"https://github.com/"),
        "url template untouched"
    );

    // Re-pinning the same values again is a byte no-op — that is how the
    // cask step detects "someone already landed this pin" after a reset.
    assert_eq!(
        publish::repin_cask_text(&pinned, "0.26.0", sha).unwrap(),
        pinned
    );
    // And the freshness probe reads the pin back.
    assert_eq!(verify::cask_version(&pinned).as_deref(), Some("0.26.0"));

    assert!(publish::repin_cask_text("cask \"aterm\" do\nend\n", "0.26.0", sha).is_err());
}

#[test]
fn cask_retry_rederives_live_digest_and_rejects_stale_attempt_mutant() {
    let old = "a".repeat(64);
    let new = "b".repeat(64);
    // Attempt one is fresh but its branch CAS is rejected by a moving main.
    publish::validate_cask_attempt_freshness(&old, &old).unwrap();
    // Production re-enters the loop and derives the changed live digest.
    publish::validate_cask_attempt_freshness(&new, &new).unwrap();
    // Negative control: hoisting the old digest outside the retry loop is stale.
    assert!(publish::validate_cask_attempt_freshness(&old, &new).is_err());
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
            build: 1_783_918_101
        }
    );

    let cli::Cmd::Cut { opts, abandon } = parse(&[
        "cut",
        "--dry-run",
        "--set-version",
        "v0.27.0",
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
    assert_eq!(opts.set_version.as_deref(), Some("0.27.0"));
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
        // --set-version now REQUIRES the canonical three-component form;
        // the retired two-component spelling is just another malformed one.
        (vec!["cut", "--set-version", "0.26"], "MAJOR.MINOR.PATCH"),
        (
            vec!["cut", "--set-version", "0.26.0.1"],
            "MAJOR.MINOR.PATCH",
        ),
        (vec!["cut", "--set-version", "01.26.0"], "MAJOR.MINOR.PATCH"),
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
        (vec!["yank", "1", "2"], "exactly one"),
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
    assert!(extra.contains("exactly"), "{extra}");

    // False must refuse before inspecting this deliberately nonexistent
    // repository. The boolean is an operator assertion, never a machine proof.
    let error = publish::run_recover_lost(
        &PathBuf::from("/definitely/not/an/aterm/repository"),
        "0.55.0",
        &owner,
        false,
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, publish::RECOVERY_STOPPED_PROCESS_REFUSAL);
    assert!(publish::RECOVERY_STOPPED_PROCESS_BANNER.contains("OPERATOR ASSERTION"));
    assert!(publish::RECOVERY_STOPPED_PROCESS_BANNER.contains("cannot cancel"));
}
