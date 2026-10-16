// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Standard padded Base64 — the shipped encoder (`aterm_codec::base64`).
fn b64(raw: &[u8]) -> String {
    aterm_codec::base64::encode(raw).expect("test key material is far below MAX_INPUT_LEN")
}

fn test_source() -> Source {
    Source {
        owner: "alabsystems".into(),
        repo: "aterm".into(),
    }
}

/// [`observe_floor_and_withdraw_at`] with the roster read just now.
fn observe_floor_and_withdraw(
    staging: &Staging,
    current_build: u64,
    floor_before: crate::manifest::Floor,
    observation: &crate::manifest::FloorObservation<'_>,
) -> Result<crate::manifest::Floor, String> {
    observe_floor_and_withdraw_at(
        staging,
        current_build,
        floor_before,
        observation,
        &crate::install::now_rfc3339(),
    )
}

/// The standing "cannot read the channel" wording must survive being read months
/// later out of `status.toml`: the consequence, every cause GitHub renders
/// identically, and the remedy — never a bare "idle", which an operator cannot tell
/// apart from "no updates available", and never a credential, which no lane reads.
#[test]
fn an_unreadable_channel_is_loud_and_actionable_not_idle() {
    let source = test_source();
    let text = unreadable_explanation(&source);
    assert!(text.contains("stays on its current version"), "{text}");
    assert!(text.contains("github.com/alabsystems/aterm"), "{text}");
    assert!(
        !text.contains("HTTP") && !text.contains("NEVER"),
        "the status code is mechanism, and the latch clears on a readable check: {text}"
    );
    // Every cause the web host cannot distinguish.
    assert!(
        text.contains("no published release"),
        "cause 1 missing: {text}"
    );
    assert!(text.contains("private"), "cause 2 missing: {text}");
    assert!(text.contains("renamed or gone"), "cause 3 missing: {text}");
    assert!(
        text.contains("reinstall it from its new location"),
        "the remedy is missing: {text}"
    );
    assert!(
        !text.contains("token") && !text.contains("ATERM_"),
        "no credential and no environment knob is ever a remedy: {text}"
    );
    assert!(
        !text.contains("repointed"),
        "the public channel has no repoint to check: {text}"
    );
    // A REPOINTED source (a development build) adds the one cause only it can have.
    let overridden = Source {
        owner: "someone-else".to_string(),
        repo: "private-aterm".to_string(),
    };
    let text = unreadable_explanation(&overridden);
    assert!(
        text.contains("github.com/someone-else/private-aterm"),
        "{text}"
    );
    assert!(text.contains("`[update] owner`/`repo`"), "{text}");
    assert!(!text.contains("token"), "{text}");
}

/// Reading the channel — with no credential at all — clears the stranded state and
/// the deferral latch the background loop reads.
#[test]
fn a_readable_channel_clears_the_strand_and_the_deferral() {
    let _serialized = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let source = test_source();
    RATE_LIMITED.store(true, Ordering::Relaxed);
    note_readable(&source);
    assert!(!rate_limited(), "a completed read clears the backoff latch");
    assert!(!crate::unreadable::is_stranded());
}

/// The candidate the check builds for this one release, shaped the way
/// [`web_release`] shapes it (the check synthesizes exactly one) — for tests
/// that exercise the roster chain over hand-built releases.
fn candidate_for(releases: Vec<Release>) -> Result<Option<AuthoritativeRelease>, String> {
    let [release] = <[Release; 1]>::try_from(releases).expect("exactly one release");
    let version = release.tag_name.trim_start_matches('v').to_string();
    let manifest_index = unique_asset_index(&release, APPCAST_ASSET)?.expect("an appcast");
    Ok(Some(AuthoritativeRelease {
        version,
        release,
        manifest_index,
    }))
}

fn candidate_manifest() -> Manifest {
    Manifest {
        schema: 1,
        version: "0.54.0".into(),
        build_number: 54,
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        sha256: "ab".repeat(32),
        dmg: "aterm-0.54.0.dmg".into(),
        url: None,
        zip: None,
        zip_sha256: None,
        min_build: None,
        machine_id: None,
        roster_seq: None,
        changelog: None,
    }
}

fn release_with_appcast(tag: &str, url: &str) -> Release {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    Release {
        tag_name: tag.into(),
        assets: vec![
            Asset {
                name: "aterm-appcast.toml".into(),
                url: url.into(),
            },
            Asset {
                name: format!("aterm-{version}.dmg"),
                url: format!("{url}-dmg"),
            },
        ],
    }
}

fn release_with_signed_appcast(tag: &str, manifest_url: &str, signature_url: &str) -> Release {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    Release {
        tag_name: tag.into(),
        assets: vec![
            Asset {
                name: "aterm-appcast.toml".into(),
                url: manifest_url.into(),
            },
            Asset {
                name: "aterm-appcast.toml.sig".into(),
                url: signature_url.into(),
            },
            Asset {
                name: format!("aterm-{version}.dmg"),
                url: format!("{manifest_url}-dmg"),
            },
        ],
    }
}

fn manifest_bytes(version: &str, build_number: u64, min_build: u64) -> Vec<u8> {
    manifest_bytes_with_dmg(
        version,
        build_number,
        min_build,
        &format!("aterm-{version}.dmg"),
    )
}

fn manifest_bytes_with_dmg(version: &str, build_number: u64, min_build: u64, dmg: &str) -> Vec<u8> {
    format!(
        "schema = 1\nversion = \"{version}\"\nbuild_number = {build_number}\n\
         sha256 = \"{}\"\ndmg = {dmg:?}\nmin_build = {min_build}\n",
        "ab".repeat(32),
    )
    .into_bytes()
}

#[test]
fn authoritative_manifest_version_must_equal_canonical_tag() {
    let authoritative = candidate_for(vec![release_with_appcast("v0.10.0", "mismatched-highest")])
        .unwrap()
        .unwrap();
    let mut urls = Vec::new();
    let mut download = |url: &str, _max_bytes: u64| {
        urls.push(url.to_string());
        match url {
            "mismatched-highest" => Ok(manifest_bytes("0.9.0", 10, 0)),
            unexpected => panic!("unexpected asset fetch: {unexpected}"),
        }
    };
    let fetched =
        fetch_authoritative_release(Some(authoritative), &mut download, &RosterPolicy::INERT);
    assert!(fetched.selected.is_none());
    assert!(fetched.manifest_rejected);
    assert_eq!(urls, ["mismatched-highest"]);
    assert_eq!(fetched.manifest_fetch_attempts, 1);
}

/// The zip is PREFERRED whenever the manifest carries a resolvable one —
/// that preference IS the fix: `hdiutil attach` cannot work in the orphaned
/// post-handoff process that most needs to stage, and `ditto` can. Every
/// other shape must still fall back to the DMG rather than stop updating,
/// because that is what keeps already-published releases installable.
#[test]
fn stage_selection_prefers_the_zip_and_falls_back_to_the_dmg() {
    const VERSION: &str = "0.54.0";
    let zip_name = format!("aterm-{VERSION}-mac.zip");
    let zip_digest = "cd".repeat(32);
    let push_zip_asset = |release: &mut Release| {
        release.assets.push(Asset {
            name: zip_name.clone(),
            url: "https://api.github.com/repos/o/r/releases/assets/9".into(),
        });
    };

    // A manifest with no zip — every release published before zip staging —
    // selects the DMG, exactly as it always did.
    let dmg_only_release = release_with_appcast("v0.54.0", "https://example/appcast");
    let dmg_only = candidate_manifest();
    let chosen = select_stage_artifact(&dmg_only_release, &dmg_only, VERSION).unwrap();
    assert_eq!(chosen.container, Container::Dmg);
    assert_eq!(chosen.name, "aterm-0.54.0.dmg");
    assert_eq!(chosen.sha256, dmg_only.sha256);
    assert_eq!(
        dmg_only_release.assets[chosen.asset_index].name,
        chosen.name
    );

    // Manifest and release both carry the zip: the zip wins, carrying ITS
    // digest (not the DMG's) as the bytes-must-match value.
    let mut zip_release = dmg_only_release.clone();
    push_zip_asset(&mut zip_release);
    let mut zipped = candidate_manifest();
    zipped.zip = Some(zip_name.clone());
    zipped.zip_sha256 = Some(zip_digest.clone());
    let chosen = select_stage_artifact(&zip_release, &zipped, VERSION).unwrap();
    assert_eq!(chosen.container, Container::Zip);
    assert_eq!(chosen.name, zip_name);
    assert_eq!(chosen.sha256, zip_digest);
    assert_eq!(zip_release.assets[chosen.asset_index].name, chosen.name);

    // Declared but unusable, four ways — each falls back, none refuses.
    let mut no_digest = zipped.clone();
    no_digest.zip_sha256 = None;
    let mut noncanonical = zipped.clone();
    noncanonical.zip = Some("aterm-mac.zip".into());
    let mut duplicate_zip_release = zip_release.clone();
    push_zip_asset(&mut duplicate_zip_release);
    for (label, release, manifest) in [
        (
            "no digest to check the bytes against",
            &zip_release,
            &no_digest,
        ),
        ("release carries no such asset", &dmg_only_release, &zipped),
        ("noncanonical zip name", &zip_release, &noncanonical),
        (
            "ambiguous duplicate assets",
            &duplicate_zip_release,
            &zipped,
        ),
    ] {
        let chosen = select_stage_artifact(release, manifest, VERSION)
            .unwrap_or_else(|error| panic!("{label} must still update: {error}"));
        assert_eq!(chosen.container, Container::Dmg, "{label}");
        assert_eq!(chosen.sha256, manifest.sha256, "{label}");
    }

    // The DMG identity proof stays unconditional: a manifest that names a
    // noncanonical DMG is refused outright even when its zip is perfect.
    let mut bad_dmg = zipped.clone();
    bad_dmg.dmg = "aterm.dmg".into();
    let error = select_stage_artifact(&zip_release, &bad_dmg, VERSION)
        .expect_err("a noncanonical DMG must refuse the whole release");
    assert!(error.contains("noncanonical DMG"), "{error}");
}

fn write_ready(staging: &Staging, build: u64, commit: &str, digest: &str) {
    let ready = Ready {
        build_number: build,
        version: format!("0.0.{build}"),
        commit: Some(commit.into()),
        dmg_sha256: digest.into(),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
}

fn write_bundle_identity(staging: &Staging, build: u64, commit: &str) {
    let contents = staging.staged_app.join("Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
             <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
        ),
    )
    .unwrap();
}

#[test]
fn corrupt_high_ready_and_deleted_stage_cannot_suppress_restage() {
    let staging = Staging::scratch("publishable");
    let manifest = candidate_manifest();
    std::fs::create_dir_all(&staging.staged_app).unwrap();

    // NEGATIVE CONTROL: an enormous parseable marker carries no canonical
    // artifact identity, so it must never permanently bypass the download.
    write_ready(&staging, 9_999, "bad", &"cd".repeat(32));
    assert!(
        !publishable_stage_covers(&staging, &manifest),
        "corrupt high Ready must fall through to download/restage"
    );

    let canonical_commit = manifest.commit.as_deref().unwrap();
    write_ready(&staging, 9_999, canonical_commit, &"cd".repeat(32));
    assert!(
        !publishable_stage_covers(&staging, &manifest),
        "canonical high Ready plus an empty app directory must still restage"
    );

    // Exact metadata without the release bundle identity is still incomplete.
    write_ready(
        &staging,
        manifest.build_number,
        canonical_commit,
        &manifest.sha256,
    );
    assert!(
        !publishable_stage_covers(&staging, &manifest),
        "exact Ready with missing Info.plist must force restage"
    );

    // A complete exact stage is the positive control for the short-circuit.
    write_bundle_identity(&staging, manifest.build_number, canonical_commit);
    assert!(publishable_stage_covers(&staging, &manifest));

    // Marker metadata without the published directory is not a stage. Both
    // pre-lock and post-lock call this predicate, so either observation falls
    // through to a fresh download/stage transaction.
    std::fs::remove_dir_all(&staging.staged_app).unwrap();
    assert!(
        !publishable_stage_covers(&staging, &manifest),
        "deleted staged_app must force download/restage"
    );

    // Same-build metadata from another artifact cannot suppress this release.
    std::fs::create_dir_all(&staging.staged_app).unwrap();
    write_ready(
        &staging,
        manifest.build_number,
        canonical_commit,
        &"ef".repeat(32),
    );
    assert!(
        !publishable_stage_covers(&staging, &manifest),
        "same build with another digest must be restaged"
    );

    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A pending, verified stage must SAY SO: both `publishable_stage_covers`
/// short-circuits record the staged decision, so `status.toml` does not end on the
/// signer-attribution note written earlier in the same check.
#[test]
fn a_stage_that_already_covers_the_candidate_records_the_staged_decision() {
    let staging = Staging::scratch("covered-status");
    let manifest = candidate_manifest();
    let commit = manifest.commit.as_deref().unwrap();
    write_ready(&staging, manifest.build_number, commit, &manifest.sha256);
    write_bundle_identity(&staging, manifest.build_number, commit);
    assert!(
        publishable_stage_covers(&staging, &manifest),
        "precondition: this stage covers the candidate"
    );

    // The signer note is what the real check writes just before the staging
    // decision, so it is the line the decision has to overwrite.
    crate::status::record(&staging, 1, "authoritative release signed by machine m3");
    record_covered_stage_status(&staging, 1, &manifest);
    let text = std::fs::read_to_string(&staging.status).expect("status written");
    assert!(
        text.contains("verified and ready to apply"),
        "the staged decision must be the last thing recorded: {text}"
    );
    assert!(
        text.contains(&format!("build {}", manifest.build_number)),
        "the staged build must be named: {text}"
    );
    assert!(
        !text.contains("signed by machine"),
        "the attribution note must not survive as the last decision: {text}"
    );

    // The marker can vanish between the predicate and the record (a concurrent
    // retire). Even then the arm records a DECISION rather than leaving the previous
    // line standing, which is the whole failure this exists to prevent.
    std::fs::remove_file(&staging.ready).unwrap();
    record_covered_stage_status(&staging, 1, &manifest);
    let text = std::fs::read_to_string(&staging.status).expect("status written");
    assert!(
        text.contains("already covers release build"),
        "the fallback must still record a decision: {text}"
    );

    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE regression this split exists for: a DOWNLOAD backoff must never hold an
/// already-staged build hostage.
///
/// The observed machine had `staged_build=1785510971`, `staged_version=0.10.0`,
/// `relaunch_ready=true` and `failing_applies=0`, and still reported "skipping
/// build 1785510971 for another 1387m (failed to stage 4 time(s))": a re-publish
/// of the same build under a NEW digest had failed to stage four times, and its
/// 24 h re-download window was also refusing to apply a bundle that was already
/// downloaded, verified, extracted and marked ready. Different failures, different
/// remedies — they must not share a timer.
/// The health half of the same arm, pinned separately because it is the half that
/// silently mis-reported healthy machines: a backed-off check must clear the
/// ACQUISITION streaks (the list, appcast and authorization all worked, so unrelated
/// blips must not accumulate toward PERSISTENT_AFTER for the whole 24 h life of the
/// memo) and must NOT clear `stage_failures` — a machine that keeps failing to stage
/// interleaves failures with backed-off checks, so resetting that streak here would
/// mean the one class whose escalation says "the bytes arrive and will not become a
/// bundle" could never reach PERSISTENT_AFTER.
#[test]
fn a_backed_off_check_clears_the_acquisition_streaks_but_never_the_stage_streak() {
    let staging = Staging::scratch("backoff-health");
    let ledger = staging.health();
    crate::health::Health::record_failure(&ledger, "network", "dns");
    crate::health::Health::record_failure(&ledger, "pipeline", "asset fetch failed");
    crate::health::Health::record_failure(&ledger, "manifest", "bad signature");
    crate::health::Health::record_failure(&ledger, "stage", "sha256 mismatch");

    let h = crate::health::Health::record_acquisition_success(&ledger);
    assert_eq!(h.network_failures, 0, "network streak must clear");
    assert_eq!(h.pipeline_failures, 0, "pipeline streak must clear");
    assert_eq!(h.manifest_failures, 0, "manifest streak must clear");
    assert_eq!(
        h.stage_failures, 1,
        "the stage streak is the one this arm must preserve"
    );

    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A STAGE THAT COULD NOT FINISH IS A MOMENT, NOT A VERDICT. A verification
/// helper past its deadline on a loaded machine, a helper the kernel would not
/// start just then, a copy past its launchd budget: each says nothing about the
/// bytes, so none opens the verdict re-download backoff or the "will not verify"
/// notice.
///
/// AND A MOMENT THAT NEVER PASSES STILL ESCALATES: each moment advances the stage
/// streak, and the persistent notice past it says staging could not
/// FINISH — never that the build will not verify. (How often a moment that keeps
/// coming back may re-download is `a_staging_moment_that_never_passes_is_throttled`.)
#[test]
fn a_stage_that_could_not_finish_books_no_verdict_but_escalates_in_its_own_words() {
    use crate::manifest::FailedMark;

    let staging = Staging::scratch("stage-passing");
    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    const NOW: u64 = 1_000_000;

    let moment = format!(
        "staged bundle failed verification: {}",
        crate::verify::timed_out("codesign --verify (team-pinned)", false)
    );
    assert_eq!(
        book_stage_failure(&staging, running, &manifest, &moment, NOW),
        StageBooked::Moment
    );
    assert!(
        stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).is_none(),
        "one moment costs nothing: the next check tries again"
    );
    for _ in 1..crate::PERSISTENT_AFTER {
        let _ = book_stage_failure(&staging, running, &manifest, &moment, NOW);
    }
    let memo = FailedMark::read(&staging.failed()).expect("the moments are counted");
    assert!(
        memo.moment && !memo.is_quarantine(),
        "never a verdict: {memo:?}"
    );
    // The streak spans the persistence window (`Health::persistent_class`).
    crate::health::backdate_streaks(&staging.health(), crate::PERSISTENT_SPAN);
    let h = crate::health::Health::read(&staging.health());
    assert_eq!(
        h.persistent_class(),
        Some(("stage", crate::PERSISTENT_AFTER)),
        "a moment that repeats escalates: {h:?}"
    );
    let (_, body) = crate::persistent_failure_notice("stage", h.stage_failures, &h, running);
    assert!(body.contains("could not finish"), "{body}");
    assert!(
        !body.contains("will not verify"),
        "nothing is known against the build: {body}"
    );
    // Named by the step that did not finish, without verdict words.
    assert!(
        body.contains("codesign --verify (team-pinned) did not finish within"),
        "{body}"
    );
    for cruft in [
        crate::PASSING_REFUSAL_KEY,
        "failed verification",
        "treating as",
        "every time",
    ] {
        assert!(!body.contains(cruft), "{cruft:?} in {body}");
    }
    // And it clears by itself, which is what tells the reader nothing needs
    // doing; the status line says the same.
    assert!(
        body.contains("retries it by itself, less often while this lasts"),
        "{body}"
    );

    // NEGATIVE CONTROL: a verdict on the bytes books the memo, and the notice
    // says the build will not verify.
    let _ = std::fs::remove_dir_all(&staging.root);
    let staging = Staging::scratch("stage-verdict");
    let verdict = "codesign --verify (team-pinned requirement) failed: code object is not \
                   signed at all";
    assert_eq!(
        book_stage_failure(&staging, running, &manifest, verdict, NOW),
        StageBooked::Verdict
    );
    let memo = FailedMark::read(&staging.failed()).expect("a verdict is memoized");
    assert_eq!(memo.attempts, 1);
    assert!(!memo.moment);
    assert!(stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).is_some());
    let h = crate::health::Health::read(&staging.health());
    assert_eq!(h.stage_failures, 1);
    let (_, body) = crate::persistent_failure_notice("stage", 3, &h, running);
    assert!(body.contains("will not verify"), "{body}");

    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE CONTAINER'S HASH ANSWERS WITHIN A BOUND. The hash runs on the checker
/// thread, which holds the process's check lane and `stage.lock`: a read that
/// never returns (a wedged volume) would keep both until relaunch, and no aterm on
/// the Mac could stage. A FIFO with no writer is that read — `shasum` blocks in `open(2)`
/// for good — and the bounded hash must kill it and answer, as a moment.
#[test]
fn the_container_hash_answers_within_its_bound() {
    let dir = Staging::scratch("hash-fifo");
    let fifo = dir.root.join("container.zip");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
        .expect("a scratch path has no NUL");
    // SAFETY: a valid NUL-terminated path; mkfifo reads it and nothing else.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
    let (tx, rx) = std::sync::mpsc::channel();
    let path = fifo.clone();
    let hasher = std::thread::spawn(move || {
        let _ = tx.send(container_sha256(
            &path,
            std::time::Duration::from_millis(500),
        ));
    });
    // A hang detector, not a latency budget.
    let answer = rx.recv_timeout(std::time::Duration::from_secs(60));
    if answer.is_err() {
        // Release the reader that never answered, so neither it nor this test's
        // thread outlives the failure: a writer's open lets `shasum` see EOF.
        let _ = std::fs::OpenOptions::new().write(true).open(&fifo);
    }
    let _ = hasher.join();
    // POSITIVE CONTROL: a file that reads answers its digest through the bound.
    let file = dir.root.join("abc");
    std::fs::write(&file, b"abc").unwrap();
    assert_eq!(
        container_sha256(&file, std::time::Duration::from_secs(60)).as_deref(),
        Ok("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    let _ = std::fs::remove_dir_all(&dir.root);
    let error = answer
        .expect("the hash step answers within its bound")
        .expect_err("a read that never finished has no digest");
    assert!(error.contains("shasum did not finish within"), "{error}");
    assert!(crate::is_passing_refusal(&error), "{error}");
}

/// A STAGING MOMENT THAT NEVER PASSES IS THROTTLED: a step that times out every
/// time (`spctl` behind a network that black-holes Gatekeeper) must not
/// re-download the whole container at every check of every aterm. Three such
/// checks, 40 minutes apart: the fourth must wait. Never on a verdict's schedule, and never
/// in a verdict's words.
#[test]
fn a_staging_moment_that_never_passes_is_throttled() {
    use crate::manifest::{FailedMark, MOMENT_BACKOFF_SECS};

    let staging = Staging::scratch("stage-moment-throttle");
    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    const NOW: u64 = 1_000_000;
    const CHECK: u64 = 40 * 60;
    let moment = crate::verify::mark_passing(
        "spctl --assess did not finish within 30s; treating as a rejection".into(),
    );
    for k in 0..3 {
        let _ = book_stage_failure(&staging, running, &manifest, &moment, NOW + k * CHECK);
    }
    let held = stage_backoff(
        &staging,
        &manifest,
        running,
        NOW + 3 * CHECK + 60,
        Asked::Schedule,
    )
    .expect("a moment that keeps coming back must not re-download at every check");
    let line = held.status_line(manifest.build_number);
    assert!(line.contains("could not finish"), "{line}");
    assert!(
        !line.contains("failed to stage"),
        "nothing is known against it: {line}"
    );

    // Bounded: however long it lasts, the wait never grows past a moment's cap,
    // which is short of a verdict's day.
    let mut at = NOW + 3 * CHECK;
    for _ in 0..8 {
        at += 24 * 60 * 60;
        let _ = book_stage_failure(&staging, running, &manifest, &moment, at);
    }
    let memo = FailedMark::read(&staging.failed()).unwrap();
    assert!(memo.moment);
    assert_eq!(memo.retry_in_secs(at), *MOMENT_BACKOFF_SECS.last().unwrap());
    assert!(memo.retry_in_secs(at) < FailedMark::backoff_secs(u32::MAX));

    // A stage that finally publishes ends the run, so the next moment is free.
    FailedMark::clear_backoff(&staging.failed());
    let _ = book_stage_failure(&staging, running, &manifest, &moment, at);
    assert!(stage_backoff(&staging, &manifest, running, at, Asked::Schedule).is_none());
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A CHECK A PERSON ASKS FOR PASSES A MOMENT'S MEMO. A disk that stayed full for
/// three checks opens the moment's re-download memo; the person who then frees
/// the space and checks for updates (the action the refusal named) is not told to
/// wait. The schedule keeps the memo; the person's check passes it, and the words
/// say so.
/// A verdict's memo and a quarantine still hold against a person.
#[test]
fn a_check_a_person_asks_for_passes_a_full_disks_memo() {
    let staging = Staging::scratch("full-disk-person");
    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    let written = "curl download failed (exit status: 23): curl: (23) Failure writing \
                   output to destination";
    let mut said = String::new();
    for _ in 0..3 {
        said = book_download_failure(&staging, running, &manifest, "zip", written, Some(1 << 20))
            .expect_err("a full disk fails the check");
    }
    let now = crate::install::unix_now_secs();
    let held = stage_backoff(&staging, &manifest, running, now, Asked::Schedule)
        .expect("PRECONDITION: a disk that stays full throttles the schedule");
    assert!(held.moment, "{held:?}");
    assert!(
        stage_backoff(&staging, &manifest, running, now, Asked::Person).is_none(),
        "the person who freed the space and asked must get the download, not: {}",
        held.status_line(manifest.build_number)
    );
    // The words name the action that does that, and promise nothing the memo
    // refuses.
    assert!(said.contains(crate::install::FREE_SPACE_REMEDY), "{said}");
    assert!(!said.contains("next check"), "{said}");
    let line = held.status_line(manifest.build_number);
    assert!(
        line.contains("at once when you check for updates"),
        "{line}"
    );

    // NEGATIVE CONTROLS. A verdict on the same artifact holds against a person.
    let verdict = "codesign --verify (team-pinned requirement) failed: code object is not \
                   signed at all";
    let _ = book_stage_failure(&staging, running, &manifest, verdict, now);
    assert!(
        stage_backoff(&staging, &manifest, running, now, Asked::Person).is_some(),
        "a verdict is a finding about the build; a person asking changes nothing"
    );
    // And a quarantine, under a moment memo of the same artifact written after it.
    crate::manifest::FailedMark::clear_backoff(&staging.failed());
    crate::manifest::quarantine_artifact(&staging, manifest.build_number, &manifest.sha256);
    for _ in 0..3 {
        let _ = crate::manifest::FailedMark::record_stage_moment(
            &staging.failed(),
            manifest.build_number,
            &manifest.sha256,
            now,
        );
    }
    let quarantined = stage_backoff(&staging, &manifest, running, now, Asked::Person)
        .expect("a quarantine holds against a person");
    assert!(quarantined.quarantined, "{quarantined:?}");
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE HASH STEP IS BOOKED BY THE ONE RULE. A `shasum` the kernel would not start
/// just then (`EAGAIN`) or one past its deadline is a moment; a missing `shasum`
/// and a digest that does not match are verdicts. The
/// refusals are the bounded helper's own words ([`container_sha256`]).
#[test]
fn the_hash_step_books_a_moment_as_a_moment_and_a_mismatch_as_a_verdict() {
    use crate::manifest::FailedMark;

    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    let digest_of = |answer: Result<String, String>| {
        let staging = Staging::scratch("stage-digest");
        let path = staging.download.join("aterm-0.54.0.zip");
        std::fs::create_dir_all(&staging.download).unwrap();
        std::fs::write(&path, b"container").unwrap();
        let end = check_container_digest(
            &staging,
            running,
            &manifest,
            &path,
            "zip",
            &manifest.sha256,
            |_| answer,
        );
        let reclaimed = !path.exists();
        (staging, end, reclaimed)
    };
    let spawn =
        |errno| crate::verify::spawn_refusal("shasum", &std::io::Error::from_raw_os_error(errno));
    for moment in [
        spawn(libc::EAGAIN),
        crate::verify::mark_passing(
            "shasum did not finish within 120s; treating as a failure".into(),
        ),
    ] {
        let (staging, end, reclaimed) = digest_of(Err(moment));
        let error = end.expect("the check ends").expect_err("as a failure");
        assert!(crate::is_passing_refusal(&error), "{error}");
        assert!(reclaimed);
        assert!(
            stage_backoff(
                &staging,
                &manifest,
                running,
                crate::install::unix_now_secs(),
                Asked::Schedule,
            )
            .is_none(),
            "a moment books no backoff against the build: {error}"
        );
        assert!(FailedMark::read(&staging.failed()).is_some_and(|memo| memo.moment));
        let _ = std::fs::remove_dir_all(&staging.root);
    }
    // NEGATIVE CONTROLS: no `shasum` at all, and a digest that does not match,
    // are verdicts on the schedule a verdict keeps.
    for verdict in [Err(spawn(libc::ENOENT)), Ok("cd".repeat(32))] {
        let (staging, end, _) = digest_of(verdict);
        let error = end.expect("the check ends").expect_err("as a failure");
        assert!(!crate::is_passing_refusal(&error), "{error}");
        let memo = FailedMark::read(&staging.failed()).expect("a verdict is memoized");
        assert!(!memo.moment, "{memo:?}");
        assert!(
            stage_backoff(
                &staging,
                &manifest,
                running,
                crate::install::unix_now_secs(),
                Asked::Schedule,
            )
            .is_some()
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }
    // And the digest that matches goes on to the stage, booking nothing.
    let (staging, end, reclaimed) = digest_of(Ok(manifest.sha256.to_ascii_uppercase()));
    assert!(end.is_none() && !reclaimed);
    assert!(FailedMark::read(&staging.failed()).is_none());
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A RELEASE WITHDRAWN WHILE IT DOWNLOADED ENDS THE CHECK HELD, NOT FAILED. A
/// sibling's check recorded the revocation of the machine that signed it (or a
/// yank, or a quarantine) while this one downloaded; the publish refused it. The
/// check ends "held: …", never "update check failed", and the download row does
/// not promise a retry the next check would refuse.
#[test]
fn a_release_withdrawn_while_it_downloaded_ends_the_check_held() {
    let manifest = Manifest {
        build_number: 7,
        version: "0.0.7".into(),
        machine_id: Some("m11".into()),
        roster_seq: Some(9),
        ..candidate_manifest()
    };
    let running = 6;
    let refused = "stage refused: build 7 was signed by machine m11, which the machine \
                   roster has revoked — withdrawn while this check was downloading it";

    let staging = Staging::scratch("stage-withdrawn");
    let revoked = vec!["m11".to_string()];
    crate::manifest::Floor::observe_and_write(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    );
    let end = end_failed_stage(&staging, running, &manifest, refused.into());
    assert_eq!(end, Ok(None), "a hold is a healthy end");
    let status = std::fs::read_to_string(&staging.status).unwrap();
    assert!(
        status.contains("held: build 7 was signed by machine m11"),
        "{status}"
    );
    assert!(crate::manifest::FailedMark::read(&staging.failed()).is_none());
    assert_eq!(
        crate::health::Health::read(&staging.health()).total_failures(),
        0
    );
    let _ = std::fs::remove_dir_all(&staging.root);

    // The quarantine is the same kind of standing record.
    let staging = Staging::scratch("stage-quarantined");
    crate::manifest::quarantine_artifact(&staging, 7, &manifest.sha256);
    let end = end_failed_stage(&staging, running, &manifest, "stage refused".into());
    assert_eq!(end, Ok(None));
    let status = std::fs::read_to_string(&staging.status).unwrap();
    assert!(status.contains("held: build 7 is quarantined"), "{status}");
    let _ = std::fs::remove_dir_all(&staging.root);

    // NEGATIVE CONTROL: a release nobody withdrew fails the check as before.
    let staging = Staging::scratch("stage-not-withdrawn");
    let end = end_failed_stage(&staging, running, &manifest, refused.into());
    assert_eq!(end, Err(refused.to_string()));
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A FULL DISK IS CHARGED TO NEITHER THE RELEASE NOR THE DOWNLOAD HOST (design
/// law L4). It meets the check lane three ways: the unpack the
/// free-space check refuses, a `ditto` whose volume filled while it copied, and
/// a download curl could not write (its stderr as measured onto a full 4 MiB
/// volume; `ditto`'s as measured there too, whose LAST line blames the archive).
/// Each is booked here by its lane's own function, past
/// `PERSISTENT_AFTER`: no memo, no backoff, and a notice that says the update
/// could not be staged, in the disk's words, with the one action said once. A
/// transport failure is still the host's.
#[test]
fn a_full_disk_is_charged_to_neither_the_release_nor_the_download_host() {
    use crate::manifest::FailedMark;
    use std::os::unix::process::ExitStatusExt as _;

    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    // The clock the download lane books by (`book_download_failure` reads it).
    let now = crate::install::unix_now_secs();
    let volume = Staging::scratch("full-disk-volume");
    let free = crate::install::free_bytes_at(&volume.root)
        .expect("PRECONDITION: the scratch volume reports its free space");
    let unpack = crate::install::refuse_without_room(
        &volume.root,
        free.saturating_add(1 << 40),
        "the update zip",
    )
    .expect_err("PRECONDITION: more than the volume holds is refused");
    let extract = crate::install::unpack_failure(
        &volume.root,
        "ditto zip extract failed",
        std::process::ExitStatus::from_raw(1 << 8),
        "ditto: /x/zx-1/aterm.app/Contents/MacOS/aterm: No space left on device\n\
         ditto: Couldn't read pkzip signature.\n",
    );
    let written = "curl download failed (exit status: 56): curl: (56) Failure writing \
                   output to destination, passed 8192 returned 1923";
    let book = |staging: &Staging, lane: &str| match lane {
        "unpack" => {
            let _ = book_stage_failure(staging, running, &manifest, &unpack, now);
        }
        "extract" => {
            let _ = book_stage_failure(staging, running, &manifest, &extract, now);
        }
        _ => {
            let _ =
                book_download_failure(staging, running, &manifest, "zip", written, Some(1 << 20));
        }
    };
    for (lane, words) in [
        ("unpack", "not enough free space"),
        ("extract", "filled while unpacking"),
        ("download", "is full"),
    ] {
        let staging = Staging::scratch("full-disk");
        book(&staging, lane);
        assert!(
            stage_backoff(
                &staging,
                &manifest,
                running,
                crate::install::unix_now_secs(),
                Asked::Schedule,
            )
            .is_none(),
            "{lane}: a full disk books no backoff against the release"
        );
        for _ in 1..crate::PERSISTENT_AFTER {
            book(&staging, lane);
        }
        // A disk that STAYS full is throttled as the moment it is — never on a
        // verdict's schedule, which would hold the release long after it frees.
        let memo = FailedMark::read(&staging.failed()).unwrap_or_else(|| panic!("{lane}: counted"));
        assert!(memo.moment, "{lane}: {memo:?}");
        assert!(
            memo.retry_in_secs(crate::install::unix_now_secs())
                <= *crate::manifest::MOMENT_BACKOFF_SECS.last().unwrap(),
            "{lane}: {memo:?}"
        );
        // The streak spans the persistence window (`Health::persistent_class`).
        crate::health::backdate_streaks(&staging.health(), crate::PERSISTENT_SPAN);
        let h = crate::health::Health::read(&staging.health());
        assert_eq!(
            h.persistent_class(),
            Some(("stage", crate::PERSISTENT_AFTER)),
            "{lane}: a disk that stays full still escalates, as staging: {h:?}"
        );
        let (_, body) = crate::persistent_failure_notice("stage", h.stage_failures, &h, running);
        assert!(
            body.contains("could not be staged") && body.contains(words),
            "{lane}: {body}"
        );
        // The one action, said once, in the disk's words, not the tool's.
        assert_eq!(body.matches("free some space").count(), 1, "{lane}: {body}");
        assert_eq!(body.matches("retries").count(), 1, "{lane}: {body}");
        for cruft in [
            "updates download",
            "tries again",
            "exit status",
            "curl",
            "ditto",
        ] {
            assert!(!body.contains(cruft), "{lane}: {cruft:?} in {body}");
        }
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    // NEGATIVE CONTROLS: a write error on a volume with room, and a quota stop,
    // are not a full disk: no "is full", no "filled", and the cause kept.
    let staging = Staging::scratch("write-error-with-room");
    let denied = book_download_failure(
        &staging,
        running,
        &manifest,
        "zip",
        "curl download failed (exit status: 23): curl: (23) Failure writing output to \
         destination",
        Some(40 << 30),
    )
    .expect_err("a download that could not be stored fails the check");
    assert!(crate::is_passing_refusal(&denied), "{denied}");
    assert!(
        !denied.contains("is full")
            && !denied.contains("free some space")
            && !denied.contains("curl")
            && denied.contains("could not be written to")
            && denied.contains("Failure writing output to destination"),
        "{denied}"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
    let quota = crate::install::unpack_failure(
        &volume.root,
        "ditto zip extract failed",
        std::process::ExitStatus::from_raw(1 << 8),
        "ditto: /x/zx-1/aterm.app/Contents/MacOS/aterm: Disc quota exceeded\n",
    );
    assert!(crate::is_passing_refusal(&quota), "{quota}");
    assert!(
        quota.contains("disk quota ran out") && !quota.contains("filled"),
        "{quota}"
    );

    // NEGATIVE CONTROL: a download the host did not serve is still the host's
    // (asked of the classifier alone: its `pipeline` arm writes the process-wide
    // delivery note, which a sibling test's status record would read).
    assert!(!aterm_update_core::download_error_is_local_write(
        "curl download failed (exit status: 56): curl: (56) Recv failure: Connection \
         reset by peer"
    ));
    let _ = std::fs::remove_dir_all(&volume.root);
}

/// A STAGE HOLDER THAT SHOWS NO SIGN OF LIFE IS SAID, NOT WAITED OUT FOREVER. An
/// aterm stopped mid-download keeps `stage.lock`. The stage holder stamps the lock
/// as it works, so a waiter tells a sibling
/// staging (the ordinary stand-down) from one that is stopped (a typed failure,
/// booked, that `update status` and the persistent notice say).
#[test]
fn a_stage_lock_held_by_a_stopped_aterm_is_a_typed_booked_failure() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let wait = std::time::Duration::from_millis(400);

    // A holder that is staging: it stamps the lock as it works.
    let staging = Staging::scratch("stage-lock-live-holder");
    let held = aterm_update_core::FileLock::acquire(&staging.stage_lock).unwrap();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stamper = {
        let (stop, path) = (std::sync::Arc::clone(&stop), staging.stage_lock.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                crate::note_checker_lock_holder(&path);
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
        })
    };
    let Err(error) = take_stage_lock(&staging, wait) else {
        panic!("the lock is held");
    };
    stop.store(true, Ordering::Relaxed);
    stamper.join().unwrap();
    assert!(error.contains("next one reads what it staged"), "{error}");
    assert_eq!(
        crate::health::Health::read(&staging.health()).total_failures(),
        0,
        "a sibling staging is no failure: {error}"
    );
    // Released by `LOCK_UN` (the guard's drop: `aterm_update_core::FileLock`),
    // and the next probe is another staging's lock.
    drop(held);
    let _ = std::fs::remove_dir_all(&staging.root);

    // A holder that is stopped: it took the lock and wrote nothing since. The
    // lock names THIS process's pid, as it does when an older build (which never
    // stamps) holds a lock this process stamped last.
    let staging = Staging::scratch("stage-lock-stopped-holder");
    let held = aterm_update_core::FileLock::acquire(&staging.stage_lock).unwrap();
    crate::note_checker_lock_holder(&staging.stage_lock);
    // The FIRST stale reading stands down unbooked: an older build staging a
    // large container on a slow link reads the same way.
    let Err(first) = take_stage_lock(&staging, wait) else {
        panic!("the lock is held");
    };
    assert!(!first.contains(STOPPED_STAGE_HOLDER), "{first}");
    assert!(!first.contains("reads what it staged"), "{first}");
    assert_eq!(
        crate::health::Health::read(&staging.health()).total_failures(),
        0,
        "one stale reading is no verdict: {first}"
    );
    // The second, finding the same stamp, is.
    let Err(error) = take_stage_lock(&staging, wait) else {
        panic!("the lock is held");
    };
    assert!(error.contains(STOPPED_STAGE_HOLDER), "{error}");
    assert!(
        !error.contains(&format!("pid {}", std::process::id())),
        "the lock's pid is this process's own, never the holder: {error}"
    );
    assert!(
        !error.contains("reads what it staged"),
        "no staging is coming: {error}"
    );
    let h = crate::health::Health::read(&staging.health());
    assert_eq!(h.pipeline_failures, 1, "booked, so it can escalate: {h:?}");
    let (_, body) = crate::persistent_failure_notice("pipeline", 3, &h, 1);
    assert!(
        body.contains("stopped") && !body.contains("pipeline is likely broken"),
        "the persistent notice names the stopped holder: {body}"
    );
    // One sentence, said once, with no count of checks that every later
    // booking would make stale.
    assert_eq!(body.matches("stage an update").count(), 1, "{body}");
    for cruft in ["stage lock:", "SIGSTOP", "two checks"] {
        assert!(!body.contains(cruft), "{cruft:?} in {body}");
    }
    // The ledger is shared with older builds on this Mac, and they find the
    // cause by this literal: rewording it turns their notice into "pipeline is
    // likely broken".
    assert!(
        error.contains("holds the update stage lock and has shown no sign of life"),
        "{error}"
    );
    // And a ledger an older build wrote still reads as the stopped holder here.
    let older = crate::health::Health {
        last_error: "stage lock: another aterm process (the lock names pid 4242) holds \
                     the update stage lock and has shown no sign of life across two \
                     checks — it may be stopped (a SIGSTOP, a debugger) or stuck; no aterm \
                     on this Mac can stage an update until it is continued or quits"
            .to_string(),
        ..h.clone()
    };
    let (_, body) = crate::persistent_failure_notice("pipeline", 3, &older, 1);
    assert!(
        body.contains("pid 4242") && !body.contains("pipeline is likely broken"),
        "{body}"
    );
    drop(held);
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// TWO STALE READINGS ARE TWO CONSECUTIVE CHECKS'. An older aterm (which never stamps the lock) stages release R slowly; this
/// build's check reads the lock stale — the first reading, unbooked. R lands, and
/// every later check finds it covered or applied and never needs the lock. Days
/// later the older aterm is again staging slowly, the lock still carries the same
/// last newer-build stamp: the next reading must not pair with the one from R's
/// episode. A check in between that did without the lock ends the run.
#[test]
fn a_stale_stage_lock_reading_pairs_only_with_the_next_check_that_needs_the_lock() {
    let wait = std::time::Duration::from_millis(400);
    let staging = Staging::scratch("stage-lock-episodes");
    let path = staging.stage_lock.clone();
    let held = aterm_update_core::FileLock::acquire(&path).unwrap();
    crate::note_checker_lock_holder(&path);
    let check = || {
        StaleStageStamps::check_started();
        let Err(error) = take_stage_lock(&staging, wait) else {
            panic!("the lock is held");
        };
        StaleStageStamps::check_ended(&path);
        error
    };
    let booked = || crate::health::Health::read(&staging.health()).pipeline_failures;

    // R's episode: one stale reading, unbooked.
    let first = check();
    assert!(!first.contains(STOPPED_STAGE_HOLDER), "{first}");
    // Checks that never needed the lock: R covered, then applied.
    for _ in 0..2 {
        StaleStageStamps::check_started();
        StaleStageStamps::check_ended(&path);
    }
    // R2's episode, same stamp: its first reading is a first reading.
    let again = check();
    assert!(!again.contains(STOPPED_STAGE_HOLDER), "{again}");
    assert_eq!(booked(), 0, "no false stopped-holder failure: {again}");
    // NEGATIVE CONTROL: the very next check that needs the lock and finds the
    // same stamp is the second consecutive reading, and is booked.
    let second = check();
    assert!(second.contains(STOPPED_STAGE_HOLDER), "{second}");
    assert_eq!(booked(), 1);
    drop(held);
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A HUNG STAGE HOLDER LOOKS AS STUCK AS A STOPPED ONE. The holder's process runs,
/// but its check hung (a download stalled on a dead read) and the window's
/// watchdog retired its generation. The beat stamps only while the holder's
/// checker is alive — the rule `checker.lock`'s holder keeps.
#[test]
fn a_stage_lock_held_by_a_hung_check_in_a_running_aterm_is_booked() {
    use crate::checker_watch::{CheckerPhase, CheckerWatch};
    static WATCH: CheckerWatch = CheckerWatch::new();
    let wait = std::time::Duration::from_millis(400);

    // `hang` is whether the holder's generation is retired (its heartbeat no
    // longer the live one) while it holds the lock. The holder's thread runs
    // throughout, stamping whatever its beat decides.
    let hold = |staging: &Staging, hang: bool| {
        let generation = WATCH.register(1_000);
        assert!(WATCH.beat(generation, CheckerPhase::Checking));
        let (taken_tx, taken_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let staging = staging.clone();
        let holder = std::thread::spawn(move || {
            let _alive = crate::CheckAliveScope::enter(crate::checker_alive(&WATCH, generation));
            let hold = take_stage_lock(&staging, wait).expect("free");
            taken_tx.send(()).unwrap();
            let _ = release_rx.recv();
            drop(hold);
        });
        taken_rx.recv().unwrap();
        if hang {
            assert!(
                WATCH.supersede(generation).is_some(),
                "the watchdog retires it"
            );
        }
        (holder, release_tx)
    };

    // NEGATIVE CONTROL: a live holder's beat keeps the waiter standing down.
    let staging = Staging::scratch("stage-lock-alive-check");
    let (holder, release) = hold(&staging, false);
    for _ in 0..2 {
        let Err(error) = take_stage_lock(&staging, wait) else {
            panic!("the lock is held");
        };
        assert!(error.contains("next one reads what it staged"), "{error}");
    }
    assert_eq!(
        crate::health::Health::read(&staging.health()).total_failures(),
        0
    );
    release.send(()).unwrap();
    holder.join().unwrap();
    let _ = std::fs::remove_dir_all(&staging.root);

    // The hung holder: running, retired, its beat silent.
    let staging = Staging::scratch("stage-lock-hung-check");
    let (holder, release) = hold(&staging, true);
    let _ = take_stage_lock(&staging, wait);
    let Err(error) = take_stage_lock(&staging, wait) else {
        panic!("the lock is held");
    };
    assert!(error.contains(STOPPED_STAGE_HOLDER), "{error}");
    assert_eq!(
        crate::health::Health::read(&staging.health()).pipeline_failures,
        1,
        "booked, so it can escalate: {error}"
    );
    release.send(()).unwrap();
    holder.join().unwrap();
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// After X was quarantined and
/// another build Y failed to stage, a channel that names X again must still
/// find X held — not download it into another crash loop.
#[test]
fn a_quarantined_build_stays_held_after_another_build_fails_to_stage() {
    use crate::manifest::FailedMark;

    let staging = Staging::scratch("quarantine-then-other-failure");
    let x = candidate_manifest();
    let running = x.build_number - 1;
    const NOW: u64 = 1_000_000;
    crate::manifest::quarantine_artifact(&staging, x.build_number, &x.sha256);
    FailedMark::record_stage_failure(&staging.failed(), x.build_number + 1, &"cd".repeat(32), NOW);
    let held =
        stage_backoff(&staging, &x, running, NOW, Asked::Schedule).expect("X is still quarantined");
    assert!(held.quarantined);
    assert!(
        held.status_line(x.build_number).contains("is quarantined"),
        "{}",
        held.status_line(x.build_number)
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn a_stage_backoff_throttles_the_restage_never_an_already_staged_newer_build() {
    use crate::manifest::{FailedMark, RETRY_BACKOFF_SECS};

    let staging = Staging::scratch("stage-backoff-vs-apply");
    let manifest = candidate_manifest();
    let canonical_commit = manifest.commit.as_deref().unwrap().to_string();
    let running = manifest.build_number - 1;
    const NOW: u64 = 1_000_000;

    // Four consecutive stage failures of this candidate: the widest window.
    for _ in 0..4 {
        FailedMark::record_stage_failure(
            &staging.failed(),
            manifest.build_number,
            &manifest.sha256,
            NOW,
        );
    }

    // Nothing staged: the window stops the re-download, and there is no apply to
    // skip — the status line has to say exactly that.
    let backoff =
        stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).expect("window open");
    assert_eq!(backoff.attempts, 4);
    assert_eq!(backoff.retry_in_secs, RETRY_BACKOFF_SECS[3]);
    assert!(backoff.applicable.is_none());
    let line = backoff.status_line(manifest.build_number);
    assert!(line.starts_with("skipping re-stage of build "), "{line}");
    assert!(line.contains("no verified stage to apply"), "{line}");

    // Now reproduce the observed machine: a published, locally verified stage for
    // a build strictly newer than the running one, while that SAME window is open.
    // Its digest differs from the manifest's (the re-publish), so the download
    // path really is still backed off — this is not the covered-stage shortcut.
    write_ready(
        &staging,
        manifest.build_number,
        &canonical_commit,
        &"ef".repeat(32),
    );
    write_bundle_identity(&staging, manifest.build_number, &canonical_commit);
    assert!(
        !publishable_stage_covers(&staging, &manifest),
        "the staged digest is not the manifest's, so the re-stage is genuinely due"
    );
    // A stage failure throttles DOWNLOADING; it must never gate APPLYING a bundle
    // that is already downloaded, verified, extracted and marked ready. Today's
    // logic returned early here and the ready build went unoffered.
    let backoff =
        stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).expect("window open");
    let applicable = backoff.applicable.as_ref().expect("apply is not gated");
    assert_eq!(applicable.build_number, manifest.build_number);
    let line = backoff.status_line(manifest.build_number);
    assert!(line.starts_with("skipping re-stage of build "), "{line}");
    assert!(line.contains("NOT skipping apply"), "{line}");

    // A marker that is not STRICTLY newer is residue, not an update: still nothing
    // to apply.
    let residue = stage_backoff(
        &staging,
        &manifest,
        manifest.build_number,
        NOW,
        Asked::Schedule,
    );
    assert!(
        residue.expect("window open").applicable.is_none(),
        "a marker for the running build is not an applicable update"
    );

    // And the window governs only its own lane's deadline: past it, the re-stage
    // resumes on its own.
    let past_deadline = NOW + RETRY_BACKOFF_SECS[3];
    assert!(
        stage_backoff(&staging, &manifest, running, past_deadline, Asked::Schedule).is_none(),
        "an expired window throttles nothing"
    );

    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A QUARANTINED BUILD NEVER COMES BACK, AND THE LINE MUST NOT PROMISE A TIMER.
///
/// `suppresses` treats a zero `retry_after` as "already elapsed", as a pre-budget
/// legacy marker means. This pins BOTH halves: the quarantine flag suppresses at
/// any future time, and an identity-only marker with a zero deadline does not (so
/// the flag is what is load-bearing, not the zero deadline).
#[test]
fn a_quarantined_build_is_skipped_forever_and_the_status_line_says_so() {
    use crate::manifest::FailedMark;

    let staging = Staging::scratch("stage-backoff-quarantine");
    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    const NOW: u64 = 1_000_000;
    const A_DECADE: u64 = 10 * 365 * 24 * 60 * 60;

    // CONTROL: an identity-only marker opens no window at all.
    FailedMark::record(&staging.failed(), manifest.build_number, &manifest.sha256);
    assert!(
        stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).is_none(),
        "a zero deadline reads as elapsed"
    );

    FailedMark::record_quarantine(&staging.failed(), manifest.build_number, &manifest.sha256);
    let backoff =
        stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).expect("quarantined");
    assert!(backoff.quarantined);
    let line = backoff.status_line(manifest.build_number);
    assert!(line.contains("is quarantined"), "{line}");
    assert!(
        !line.contains("retrying automatically") && !line.contains("for another "),
        "a quarantine has no clock; the line must not imply one: {line}"
    );
    assert!(
        line.contains("will not retry it"),
        "and must say plainly that nothing here will: {line}"
    );

    // No deadline can lapse it.
    assert!(
        stage_backoff(
            &staging,
            &manifest,
            running,
            NOW + A_DECADE,
            Asked::Schedule
        )
        .is_some(),
        "a quarantine does not expire"
    );

    // THE ESCAPES: a different build, and a re-publish of the same build under a
    // different digest, both miss the memo's key and stage normally.
    let newer = Manifest {
        build_number: manifest.build_number + 1,
        ..candidate_manifest()
    };
    assert!(
        stage_backoff(&staging, &newer, running, NOW, Asked::Schedule).is_none(),
        "a newer build is not the quarantined artifact"
    );
    let republished = Manifest {
        sha256: "cd".repeat(32),
        ..candidate_manifest()
    };
    assert_ne!(
        republished.sha256, manifest.sha256,
        "the re-publish fixture must actually differ or it proves nothing"
    );
    assert!(
        stage_backoff(&staging, &republished, running, NOW, Asked::Schedule).is_none(),
        "a re-publish under a different digest is not the quarantined artifact"
    );

    let _ = std::fs::remove_dir_all(&staging.root);
}

/// Every removal on the download path is keyed to the CURRENT artifact's name, and
/// those names carry the version — so a `.part` (or a whole container abandoned
/// between the finalize rename and the post-stage removal) for a version the channel
/// has moved past is unreachable by every other code path. The sweep is
/// the only reclaim, and it runs under the stage lock before the next download.
#[test]
fn download_scratch_from_a_version_the_channel_moved_past_is_reclaimed() {
    let staging = Staging::scratch("download-sweep");

    // Two abandoned versions plus the current one: a killed transfer's `.part` and
    // a fully-downloaded container that was never staged.
    let stale_part = staging.download.join("aterm-0.52.0-mac.zip.part");
    let stale_container = staging.download.join("aterm-0.53.0-mac.zip");
    let current_part = staging.download.join("aterm-0.54.0-mac.zip.part");
    for leftover in [&stale_part, &stale_container, &current_part] {
        std::fs::write(leftover, b"abandoned bytes").unwrap();
    }
    // A directory is not scratch we own; the sweep must not recurse into one.
    let bystander = staging.download.join("not-ours");
    std::fs::create_dir_all(bystander.join("keep")).unwrap();

    sweep_download_scratch(&staging);

    assert!(
        !stale_part.exists(),
        "a killed transfer's part file for a superseded version has no other reclaimer"
    );
    assert!(
        !stale_container.exists(),
        "an abandoned full container for a superseded version has no other reclaimer"
    );
    assert!(
        !current_part.exists(),
        "the sweep subsumes the pre-download remove_file it replaced"
    );
    assert!(
        bystander.join("keep").is_dir(),
        "the sweep removes regular files only, never a directory tree"
    );

    // Idempotent, and silent on a download dir that does not exist yet (a first
    // check on a fresh machine reaches it before anything has created the dir).
    sweep_download_scratch(&staging);
    let _ = std::fs::remove_dir_all(&staging.root);
    sweep_download_scratch(&staging);
}

// -----------------------------------------------------------------------
// The machine-roster tier, on the real client transport path.
//
// The chain's own gates are proved in `aterm_update_core::roster`; what these
// exercise is the WIRING — that the assets are demanded, fetched, sequenced in the
// documented order, that attribution comes back out, and that every refusal is a
// refusal rather than a fallthrough.
// -----------------------------------------------------------------------

/// Obviously synthetic seeds, distinct from `SIGNING_SEED` so a mix-up between the
/// channel key and a machine key cannot pass by coincidence.
const MASTER_SEED_FIXTURE: [u8; 32] = [0xA7; 32];
const M3_SEED_FIXTURE: [u8; 32] = [0xB7; 32];

/// 2026-08-04T00:00:00Z.
const ROSTER_NOW: i64 = 1_785_801_600;

fn release_with_roster(tag: &str) -> Release {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let mut release = release_with_signed_appcast(tag, "m-url", "sig-url");
    release.assets.push(Asset {
        name: "aterm-machines.toml".into(),
        url: "roster-url".into(),
    });
    release.assets.push(Asset {
        name: "aterm-machines.toml.sig".into(),
        url: "roster-sig-url".into(),
    });
    assert!(
        release
            .assets
            .iter()
            .any(|a| a.name == format!("aterm-{version}.dmg"))
    );
    release
}

/// An appcast carrying the two attribution keys, signed by `machine`.
fn attributed_manifest(machine_id: &str, roster_seq: u64) -> Vec<u8> {
    let mut text = String::from_utf8(manifest_bytes("0.10.0", 10, 0)).unwrap();
    text.push_str(&format!(
        "machine_id = {machine_id:?}\nroster_seq = {roster_seq}\n"
    ));
    text.into_bytes()
}

/// The full owner side: a master-signed roster listing m3, plus everything the client
/// needs to check it.
struct RosterFixture {
    master_pub: String,
    roster: Vec<u8>,
    roster_sig: Vec<u8>,
    manifest: Vec<u8>,
    manifest_sig: Vec<u8>,
    machine_pub: String,
    seq: u64,
}

fn roster_fixture(revoke_m3: bool) -> RosterFixture {
    let master = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE).unwrap();
    let m3 = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
    let machine_pub = b64(m3.public_key().as_ref());
    let seq = 4u64;
    let roster = aterm_update_core::roster::Roster {
        schema: 1,
        roster_seq: seq,
        valid_until: "2027-02-01T00:00:00Z".into(),
        machines: vec![aterm_update_core::roster::Machine {
            id: "m3".into(),
            pubkey: machine_pub.clone(),
            added_at: "2026-08-04T00:00:00Z".into(),
            not_after: None,
        }],
        revoked: if revoke_m3 { vec!["m3".into()] } else { vec![] },
    };
    let roster_bytes = roster.to_toml().unwrap().into_bytes();
    let manifest = attributed_manifest("m3", seq);
    RosterFixture {
        master_pub: b64(master.public_key().as_ref()),
        roster_sig: master.sign(&roster_bytes).as_ref().to_vec(),
        roster: roster_bytes,
        manifest_sig: m3.sign(&manifest).as_ref().to_vec(),
        manifest,
        machine_pub,
        seq,
    }
}

/// THE CLIENT HAPPY PATH: the roster assets are fetched, the chain passes, the release
/// is selected, and the attribution names the machine that signed it.
#[test]
fn an_armed_master_accepts_a_rostered_release_and_reports_which_machine_signed() {
    let f = roster_fixture(false);
    let selected = candidate_for(vec![release_with_roster("v0.10.0")])
        .unwrap()
        .unwrap();
    let master = [f.master_pub.as_str()];
    let mut urls = Vec::new();
    let mut download = |url: &str, _max: u64| {
        urls.push(url.to_string());
        match url {
            "m-url" => Ok(f.manifest.clone()),
            "sig-url" => Ok(f.manifest_sig.clone()),
            "roster-url" => Ok(f.roster.clone()),
            "roster-sig-url" => Ok(f.roster_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        }
    };
    let _ = crate::log_capture::take();
    let fetched = fetch_authoritative_release(
        Some(selected),
        &mut download,
        &RosterPolicy {
            master_pubkeys: &master,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(
        fetched.selected.is_some(),
        "the chain must accept this release"
    );
    let who = fetched.attribution.expect("attribution is reported");
    assert_eq!(who.machine_id, "m3");
    assert_eq!(who.pubkey_b64, f.machine_pub);
    assert_eq!(who.roster_seq, f.seq);
    assert_eq!(
        fetched.observed_roster_seq,
        Some(f.seq),
        "the accepted sequence must reach the caller so the durable floor ratchets"
    );
    assert!(
        urls.contains(&"roster-url".to_string()) && urls.contains(&"roster-sig-url".to_string()),
        "the roster and its master signature must actually be fetched: {urls:?}"
    );
    // The LOG contract of an accept: one DEBUG attribution line, nothing at WARN —
    // and not again for the same release in the same process.
    let lines = crate::log_capture::take();
    assert!(
        lines
            .iter()
            .all(|(level, _)| *level != aterm_log::Level::Warn),
        "an accepted release must log nothing at WARN: {lines:?}"
    );
    let attributed: Vec<_> = lines
        .iter()
        .filter(|(_, msg)| msg.contains("was signed by machine"))
        .collect();
    assert_eq!(
        attributed.len(),
        1,
        "attribution is stated exactly once: {lines:?}"
    );
    assert_eq!(
        attributed[0].0,
        aterm_log::Level::Debug,
        "the note is DEBUG: {lines:?}"
    );
    let mut download_again = |url: &str, _max: u64| match url {
        "m-url" => Ok(f.manifest.clone()),
        "sig-url" => Ok(f.manifest_sig.clone()),
        "roster-url" => Ok(f.roster.clone()),
        "roster-sig-url" => Ok(f.roster_sig.clone()),
        other => Err(format!("unexpected fetch {other}")),
    };
    let again = fetch_authoritative_release(
        Some(
            candidate_for(vec![release_with_roster("v0.10.0")])
                .unwrap()
                .unwrap(),
        ),
        &mut download_again,
        &RosterPolicy {
            master_pubkeys: &master,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(again.selected.is_some(), "the release is still accepted");
    let lines = crate::log_capture::take();
    assert!(
        !lines
            .iter()
            .any(|(_, msg)| msg.contains("was signed by machine")),
        "once per release per process: {lines:?}"
    );
}

/// A RELEASE WITH NO ROSTER is refused under an armed master — structurally, before
/// any roster crypto. An armed anchor never degrades to "unsigned is fine".
#[test]
fn an_armed_master_refuses_a_release_that_carries_no_roster() {
    let f = roster_fixture(false);
    // The plain signed release: appcast + signature, no roster assets.
    let selected = candidate_for(vec![release_with_signed_appcast(
        "v0.10.0", "m-url", "sig-url",
    )])
    .unwrap()
    .unwrap();
    let master = [f.master_pub.as_str()];
    let mut download = |url: &str, _max: u64| match url {
        "m-url" => Ok(f.manifest.clone()),
        "sig-url" => Ok(f.manifest_sig.clone()),
        other => panic!("nothing else may be fetched, got {other}"),
    };
    let fetched = fetch_authoritative_release(
        Some(selected),
        &mut download,
        &RosterPolicy {
            master_pubkeys: &master,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(fetched.selected.is_none());
    assert!(fetched.manifest_rejected);
    assert!(fetched.attribution.is_none());
}

/// A REVOKED MACHINE is refused on the real path, though its signature is genuine.
/// This is the whole point of the tier.
#[test]
fn a_revoked_machine_is_refused_on_the_client_transport_path() {
    let f = roster_fixture(true);
    let selected = candidate_for(vec![release_with_roster("v0.10.0")])
        .unwrap()
        .unwrap();
    let master = [f.master_pub.as_str()];
    let mut download = |url: &str, _max: u64| match url {
        "m-url" => Ok(f.manifest.clone()),
        "sig-url" => Ok(f.manifest_sig.clone()),
        "roster-url" => Ok(f.roster.clone()),
        "roster-sig-url" => Ok(f.roster_sig.clone()),
        other => Err(format!("unexpected fetch {other}")),
    };
    let _ = crate::log_capture::take();
    let fetched = fetch_authoritative_release(
        Some(selected),
        &mut download,
        &RosterPolicy {
            master_pubkeys: &master,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(
        fetched.selected.is_none(),
        "a revoked machine must not publish"
    );
    assert!(fetched.manifest_rejected);
    assert!(fetched.attribution.is_none());
    // The contrast that keeps the rotation note's INFO demotion honest: an actual
    // refusal — the roster does NOT authorize the signer — still surfaces at WARN.
    let lines = crate::log_capture::take();
    assert!(
        lines
            .iter()
            .any(|(level, msg)| *level == aterm_log::Level::Warn
                && msg.contains("refusing authoritative")),
        "a roster refusal is verification degradation and must WARN: {lines:?}"
    );
}

/// A REPLAYED PRE-REVOCATION ROSTER is refused by the durable floor, and a STALE one
/// by the freshness window. Both are checked before any artifact crypto, and both are
/// driven here through the real transport path.
#[test]
fn a_rolled_back_or_lapsed_roster_is_refused_on_the_client_transport_path() {
    let f = roster_fixture(false);
    let master = [f.master_pub.as_str()];
    let refuse_with = |floor_seq: u64, now_unix: i64| {
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(f.manifest.clone()),
            "sig-url" => Ok(f.manifest_sig.clone()),
            "roster-url" => Ok(f.roster.clone()),
            "roster-sig-url" => Ok(f.roster_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        };
        fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &master,
                floor_seq,
                now_unix,
                floor_refresh: None,
            },
        )
    };
    // A client that has durably seen sequence 5 refuses this seq-4 roster forever.
    let rolled_back = refuse_with(f.seq + 1, ROSTER_NOW);
    assert!(rolled_back.selected.is_none() && rolled_back.manifest_rejected);
    assert_eq!(
        rolled_back.observed_roster_seq, None,
        "a rolled-back roster failed admission: not an observation, moves no floor"
    );
    // Past `valid_until`, the same roster is refused even with no floor at all — the
    // only defence a fresh install has.
    let lapsed = refuse_with(0, 1_900_000_000);
    assert!(lapsed.selected.is_none() && lapsed.manifest_rejected);
    assert_eq!(
        lapsed.observed_roster_seq, None,
        "a stale roster failed admission: not an observation, moves no floor"
    );
    // Negative control: at the same sequence and inside the window it is accepted, so
    // the two refusals above are the gates and not a broken fixture.
    assert!(refuse_with(f.seq, ROSTER_NOW).selected.is_some());
}

/// A GENUINE SIGNATURE WITH A MISMATCHED LABEL is refused after the parse. The bytes
/// verify under m3's key, but they claim to come from `m99`, and attribution follows
/// the key.
#[test]
fn a_release_whose_declared_machine_disagrees_with_the_signer_is_refused() {
    let master = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE).unwrap();
    let m3 = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
    let machine_pub = b64(m3.public_key().as_ref());
    let roster = aterm_update_core::roster::Roster {
        schema: 1,
        roster_seq: 4,
        valid_until: "2027-02-01T00:00:00Z".into(),
        machines: vec![aterm_update_core::roster::Machine {
            id: "m3".into(),
            pubkey: machine_pub.clone(),
            added_at: String::new(),
            not_after: None,
        }],
        revoked: vec![],
    };
    let roster_bytes = roster.to_toml().unwrap().into_bytes();
    let roster_sig = master.sign(&roster_bytes).as_ref().to_vec();
    // m3 signs bytes that CLAIM to be m99's.
    let lying = attributed_manifest("m99", 4);
    let lying_sig = m3.sign(&lying).as_ref().to_vec();

    let master_pub = b64(master.public_key().as_ref());
    let masters = [master_pub.as_str()];
    let selected = candidate_for(vec![release_with_roster("v0.10.0")])
        .unwrap()
        .unwrap();
    let mut download = |url: &str, _max: u64| match url {
        "m-url" => Ok(lying.clone()),
        "sig-url" => Ok(lying_sig.clone()),
        "roster-url" => Ok(roster_bytes.clone()),
        "roster-sig-url" => Ok(roster_sig.clone()),
        other => Err(format!("unexpected fetch {other}")),
    };
    let fetched = fetch_authoritative_release(
        Some(selected),
        &mut download,
        &RosterPolicy {
            master_pubkeys: &masters,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(
        fetched.selected.is_none() && fetched.manifest_rejected,
        "a signature cannot be relabelled onto another machine's identity"
    );
}

/// AN UNPINNED MASTER (a fork with no master of its own) REMOVES THE SIGNATURE TIER:
/// only the appcast is fetched — no signature, no roster — no attribution is
/// produced, and the release proceeds on the channel repository, the sha256 and the
/// codesign checks. Exercised with a synthetic empty anchor; this tree is armed.
#[test]
fn an_unpinned_master_never_touches_the_signature_or_the_roster() {
    let f = roster_fixture(false);
    let selected = candidate_for(vec![release_with_roster("v0.10.0")])
        .unwrap()
        .unwrap();
    let mut urls = Vec::new();
    let mut download = |url: &str, _max: u64| {
        urls.push(url.to_string());
        match url {
            "m-url" => Ok(f.manifest.clone()),
            other => panic!("an absent tier must fetch nothing else, got {other}"),
        }
    };
    let fetched = fetch_authoritative_release(Some(selected), &mut download, &RosterPolicy::INERT);
    assert!(fetched.selected.is_some());
    assert!(fetched.attribution.is_none());
    assert_eq!(fetched.observed_roster_seq, None);
    assert_eq!(urls, ["m-url"]);
}

// -----------------------------------------------------------------------
// THE ROSTER IS THE AUTHORITY.
//
// Everything below drives the REAL transport path; the chain's own gates are
// proved in `aterm_update_core::roster`.
// -----------------------------------------------------------------------

/// A second master, obviously synthetic and distinct from everything above so a
/// mix-up cannot pass by coincidence.
const OTHER_MASTER_FIXTURE: [u8; 32] = [0xA8; 32];

/// Everything the owner publishes for one release, with every placement under the
/// caller's control — which machine is on the roster, which is revoked, which one
/// signed, and at which generation. The placements ARE the subject of these tests.
#[derive(Clone)]
struct Chain {
    master_pub: String,
    roster: Vec<u8>,
    roster_sig: Vec<u8>,
    manifest: Vec<u8>,
    manifest_sig: Vec<u8>,
}

fn pub_b64(seed: &[u8; 32]) -> String {
    b64(Ed25519KeyPair::from_seed_unchecked(seed)
        .unwrap()
        .public_key()
        .as_ref())
}

fn chain(
    machines: &[(&str, [u8; 32])],
    revoked: &[&str],
    signer: (&str, [u8; 32]),
    seq: u64,
    master_seed: &[u8; 32],
    claimed_seq: u64,
) -> Chain {
    let master = Ed25519KeyPair::from_seed_unchecked(master_seed).unwrap();
    let roster = aterm_update_core::roster::Roster {
        schema: 1,
        roster_seq: seq,
        valid_until: "2027-02-01T00:00:00Z".into(),
        machines: machines
            .iter()
            .map(|(id, seed)| aterm_update_core::roster::Machine {
                id: (*id).to_string(),
                pubkey: pub_b64(seed),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            })
            .collect(),
        revoked: revoked.iter().map(|s| (*s).to_string()).collect(),
    };
    let roster_bytes = roster.to_toml().unwrap().into_bytes();
    let manifest = attributed_manifest(signer.0, claimed_seq);
    let signing = Ed25519KeyPair::from_seed_unchecked(&signer.1).unwrap();
    Chain {
        master_pub: b64(master.public_key().as_ref()),
        roster_sig: master.sign(&roster_bytes).as_ref().to_vec(),
        roster: roster_bytes,
        manifest_sig: signing.sign(&manifest).as_ref().to_vec(),
        manifest,
    }
}

/// Drive the real path over a [`Chain`] with an explicit master policy. Returns both
/// the verdict and the URLs that were actually fetched.
fn run_chain(
    c: &Chain,
    masters: &[&str],
    floor_seq: u64,
    now_unix: i64,
) -> (AuthoritativeFetch, Vec<String>) {
    let selected = candidate_for(vec![release_with_roster("v0.10.0")])
        .unwrap()
        .unwrap();
    let mut urls = Vec::new();
    let mut download = |url: &str, _max: u64| {
        urls.push(url.to_string());
        match url {
            "m-url" => Ok(c.manifest.clone()),
            "sig-url" => Ok(c.manifest_sig.clone()),
            "roster-url" => Ok(c.roster.clone()),
            "roster-sig-url" => Ok(c.roster_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        }
    };
    let fetched = fetch_authoritative_release(
        Some(selected),
        &mut download,
        &RosterPolicy {
            master_pubkeys: masters,
            floor_seq,
            now_unix,
            floor_refresh: None,
        },
    );
    (fetched, urls)
}

/// THE OBSERVATION-RATCHET SEMANTICS, pinned end to end so the code and its comment
/// can never again disagree about when the floor moves. "A client that merely SAW
/// roster generation n must refuse n-1 forever after" means the observation is
/// reported on ADMISSION — not on successful artifact authorization — and exactly on
/// admission: a roster that never passed `admit` (stale, rolled back, unverifiable)
/// has NOT been observed and moves nothing.
///
/// MUTATION: move the `observed_roster_seq` assignment back inside the `Ok(who)` arm
/// of `authorize_by_roster`'s caller (the pre-fix code) and the first half fails; make
/// it fire before `admit` and the second half fails.
#[test]
fn the_roster_floor_ratchets_on_observation_not_on_acceptance() {
    // ACT 1 — the attack the ratchet exists for. Generation 10 revokes m3; m3 itself
    // signed the release. The release is refused AND generation 10 is observed.
    let revoking = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &["m3"],
        ("m3", M3_SEED_FIXTURE),
        10,
        &MASTER_SEED_FIXTURE,
        10,
    );
    let masters_owned = revoking.master_pub.clone();
    let masters = [masters_owned.as_str()];
    let (saw_revocation, _) = run_chain(&revoking, &masters, 9, ROSTER_NOW);
    assert!(saw_revocation.selected.is_none() && saw_revocation.manifest_rejected);
    assert_eq!(
        saw_revocation.observed_roster_seq,
        Some(10),
        "observing the revoking generation must be reported for the durable ratchet"
    );

    // ACT 2 — the replay, against the floor ACT 1's observation produced. The seq-9
    // roster still lists m3 and is still inside its freshness window; with the floor
    // at 10 it must be refused, and it is NOT an observation (admit failed).
    let pre_revocation = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        9,
        &MASTER_SEED_FIXTURE,
        9,
    );
    let floor_after_observation = saw_revocation.observed_roster_seq.unwrap();
    let (replayed, _) = run_chain(
        &pre_revocation,
        &masters,
        floor_after_observation,
        ROSTER_NOW,
    );
    assert!(
        replayed.selected.is_none() && replayed.manifest_rejected,
        "the replayed pre-revocation roster must be refused by the observed floor"
    );
    assert_eq!(
        replayed.observed_roster_seq, None,
        "a roster that failed admission was never observed and must not move the floor"
    );

    // NEGATIVE CONTROL — the same seq-9 roster is accepted below the floor ACT 1
    // produced, so ACT 2's refusal is the ratchet's doing and not a broken fixture.
    let (accepted, _) = run_chain(&pre_revocation, &masters, 9, ROSTER_NOW);
    assert!(accepted.selected.is_some());
    assert_eq!(accepted.observed_roster_seq, Some(9));
}

/// THE ADMISSION-TIME FLOOR RE-READ (the check-vs-ratchet TOCTOU): the policy's
/// `floor_seq` snapshot is taken before any network I/O, and a concurrent instance
/// can ratchet the durable floor while the roster assets download. When the caller
/// provides `floor_refresh`, admission must consult the RE-READ value, so a roster
/// generation a concurrent check has already superseded is refused even though the
/// stale snapshot would admit it.
///
/// MUTATION: drop the `floor_refresh` consultation in `authorize_by_roster` (admit
/// against `policy.floor_seq` alone — the pre-fix code) and the refusal below flips
/// to an acceptance.
#[test]
fn admission_rereads_the_durable_floor_a_concurrent_check_may_have_ratcheted() {
    let c = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        9,
        &MASTER_SEED_FIXTURE,
        9,
    );
    let masters = [c.master_pub.as_str()];
    let run_with_refresh = |refreshed_floor: u64| {
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(c.manifest.clone()),
            "sig-url" => Ok(c.manifest_sig.clone()),
            "roster-url" => Ok(c.roster.clone()),
            "roster-sig-url" => Ok(c.roster_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        };
        // The concurrent actor's ratchet, visible only through the re-read: the
        // snapshot below stays at 0, exactly as in the race.
        let refresh = move || refreshed_floor;
        fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &masters,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: Some(&refresh),
            },
        )
    };
    // The other instance recorded generation 10 mid-download: this seq-9 roster is
    // superseded and must be refused, stale snapshot notwithstanding.
    let raced = run_with_refresh(10);
    assert!(
        raced.selected.is_none() && raced.manifest_rejected,
        "admission must honour the re-read floor, not the pre-download snapshot"
    );
    assert_eq!(raced.observed_roster_seq, None);
    // NEGATIVE CONTROL: with the durable floor still at 9, the same chain is
    // admitted — so the refusal above is the re-read's doing.
    let quiet = run_with_refresh(9);
    assert!(quiet.selected.is_some());
    assert_eq!(quiet.observed_roster_seq, Some(9));
}

/// The under-stage-lock half of the same defence: whether an already-authorized
/// release must be dropped because the durable floor advanced past its generation
/// while the check was in flight. Strictly `<` — this run's own ratchet write makes
/// the floor EQUAL in the quiescent case, and an inert tier (no observation) can
/// never be superseded.
/// A REVOCATION MUST REACH AN ALREADY-STAGED BUILD, and must reach nothing else.
///
/// The stage lane refuses a release whose signer the roster withdrew, but a build
/// already on disk was authorized earlier and nothing revisited it: staged at 10:00
/// by a machine revoked at 10:30, applied anyway (in-session, or at the next launch).
#[test]
fn a_revoked_signer_withdraws_the_stage_it_authorized_and_no_other() {
    let revoked = vec!["m11".to_string(), "m19".to_string()];
    assert!(revocation_withdraws_stage(&revoked, Some("m11")));
    assert!(revocation_withdraws_stage(&revoked, Some("m19")));
    // The machine that signed this stage is still on the roster.
    assert!(!revocation_withdraws_stage(&revoked, Some("m3")));
    // Nothing revoked at all is the overwhelmingly common case.
    assert!(!revocation_withdraws_stage(&[], Some("m3")));
    // A marker written before the attribution fields existed names no machine. It
    // must be LEFT ALONE: "I cannot tell who signed this" is not evidence of
    // withdrawal, and retiring on it would re-download on every check forever —
    // the same never-updates shape the apply-lane seq gate had to be removed for.
    assert!(!revocation_withdraws_stage(&revoked, None));
}

/// A published stage of `build` signed by `machine`, as the stager leaves it.
fn publish_attributed_stage(staging: &Staging, build: u64, machine: &str) {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let ready = Ready {
        build_number: build,
        version: format!("0.0.{build}"),
        commit: Some(commit.into()),
        dmg_sha256: "ab".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: Some(machine.into()),
        roster_seq: Some(9),
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
    write_bundle_identity(staging, build, commit);
}

/// A STOLEN KEY'S FLOORS GO WITH THE KEY.
///
/// Machine m11's key is stolen. The thief publishes a release whose signed
/// manifest says `build_number = min_build = 9_999_999_999`; every client that
/// SEES it ratchets its operator floor there before any download, and one that
/// stages it raises its high-water there too. The owner revokes m11 and cuts
/// the next genuine release from m3 with an ordinary build number, which must not
/// be held below the thief's floor or high-water: the floors carry provenance and
/// are withdrawn with the key.
#[test]
fn a_revoked_machines_floors_are_withdrawn_so_the_next_genuine_release_is_not_held() {
    let staging = Staging::scratch("floor-revocation");
    let floor = || crate::manifest::Floor::read(&staging.floor());
    // An ordinary history: m3's releases raised a floor and a high-water.
    crate::manifest::Floor::observe_and_write(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            min_build: 900,
            high_water: 1_500,
            roster_seq: 8,
            machine: Some("m3"),
            revoked: &[],
        },
    );
    // THE THEFT, observed under roster generation 9, which still lists m11.
    let after_theft = observe_floor_and_withdraw(
        &staging,
        1_000,
        floor(),
        &crate::manifest::FloorObservation {
            min_build: 9_999_999_999,
            roster_seq: 9,
            machine: Some("m11"),
            ..crate::manifest::FloorObservation::default()
        },
    )
    .unwrap();
    assert_eq!(after_theft.min_build, 9_999_999_999, "observed, as before");
    // …and the thief's build staged here (the stager's high-water ratchet).
    crate::manifest::Floor::observe_and_write(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            high_water: 9_000_000_000,
            machine: Some("m11"),
            ..crate::manifest::FloorObservation::default()
        },
    );
    publish_attributed_stage(&staging, 9_000_000_000, "m11");
    assert!(
        floor_hold(2_000, floor().min_build, floor().high_water).is_some(),
        "the genuine release is held while m11 is trusted"
    );

    // THE REVOCATION: generation 10 revokes m11, and m3 cuts build 2_000.
    let revoked = vec!["m11".to_string()];
    let after = observe_floor_and_withdraw(
        &staging,
        1_000,
        floor(),
        &crate::manifest::FloorObservation {
            min_build: 0,
            roster_seq: 10,
            machine: Some("m3"),
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    )
    .unwrap();
    assert_eq!(
        (after.min_build, after.high_water, after.roster_seq),
        (900, 1_500, 10),
        "the floors fall back to what the machines still trusted asked for — \
         never lower, and the roster generation never moves back: {after:?}"
    );
    assert_eq!(after, floor(), "the check judges by the floor it wrote");
    assert_eq!(
        floor_hold(2_000, after.min_build, after.high_water),
        None,
        "the owner's release after the revocation is not held"
    );
    // A genuine yank and a genuine rollback floor still stand.
    assert!(floor_hold(800, after.min_build, after.high_water).is_some());
    assert!(floor_hold(1_400, after.min_build, after.high_water).is_some());
    // The revoked machine's stage went with it (finding 10's other half).
    assert!(Ready::read_published_bundle(&staging).is_none());

    // The revoked machine's word never counts again, even replayed.
    crate::manifest::Floor::observe_and_write(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            min_build: 9_999_999_999,
            high_water: 9_000_000_000,
            machine: Some("m11"),
            ..crate::manifest::FloorObservation::default()
        },
    );
    assert_eq!((floor().min_build, floor().high_water), (900, 1_500));
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A STAGE THAT IS GONE IS NO UPDATE TO HAND OFF. Build 20,
/// signed by m11, is armed and its pre-verification pass cached; a check then
/// admits a roster revoking m11 and retires the stage — ready marker and bundle.
/// The worker that skips its full check on the cached pass must not launch a
/// successor for a stage that no longer exists. NEGATIVE CONTROL: the
/// stage still there, under a trusted signer, is let on.
#[test]
fn a_cached_pass_does_not_hand_off_to_a_stage_the_check_retired() {
    let staging = Staging::scratch("cached-pass-retired");
    let commit = "0123456789abcdef0123456789abcdef01234567";
    publish_attributed_stage(&staging, 20, "m11");
    assert_eq!(
        crate::install::handoff_cached_pass_refusal_at(&staging, 20, commit),
        None,
        "PRECONDITION: the published stage, trusted, is let on"
    );
    let revoked = vec!["m11".to_string()];
    observe_floor_and_withdraw(
        &staging,
        10,
        crate::manifest::Floor::read(&staging.floor()),
        &crate::manifest::FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    )
    .unwrap();
    assert!(
        !staging.ready.exists(),
        "PRECONDITION: the check retired it"
    );
    let refusal = crate::install::handoff_cached_pass_refusal_at(&staging, 20, commit);
    assert!(
        refusal
            .as_deref()
            .is_some_and(|why| why.contains("20") && crate::is_passing_refusal(why)),
        "a retired stage is refused, as the stage's state (a moment): {refusal:?}"
    );
    // A stage REPLACED by another build is refused as the stage's state too; one
    // replaced by another COMMIT of the build is a verdict, as the full check's is.
    publish_attributed_stage(&staging, 21, "m3");
    assert!(
        crate::install::handoff_cached_pass_refusal_at(&staging, 20, commit)
            .is_some_and(|why| crate::is_passing_refusal(&why))
    );
    assert!(
        crate::install::handoff_cached_pass_refusal_at(&staging, 21, "fedcba9876543210")
            .is_some_and(|why| !crate::is_passing_refusal(&why))
    );
    assert_eq!(
        crate::install::handoff_cached_pass_refusal_at(&staging, 21, commit),
        None
    );
    // The marker still names it, but its bundle left the disk (a retirement that
    // took the bundle first): the stage's state again, a moment,
    // and still refused.
    std::fs::remove_dir_all(&staging.staged_app).unwrap();
    let refusal = crate::install::handoff_cached_pass_refusal_at(&staging, 21, commit);
    assert!(
        refusal.as_deref().is_some_and(|why| {
            why.contains("no longer the published stage") && crate::is_passing_refusal(why)
        }),
        "{refusal:?}"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A stage of build 20 signed by `machine`, its roster authorization last
/// confirmed at `confirmed` (unix seconds).
fn publish_stage_confirmed_at(staging: &Staging, machine: &str, confirmed: u64) -> Ready {
    publish_attributed_stage(staging, 20, machine);
    let mut ready = Ready::read(&staging.ready).unwrap();
    ready.roster_confirmed_at = Some(aterm_types::rfc3339::format_rfc3339(confirmed));
    std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
    ready
}

/// THE COLD LAUNCH WAITS, AND THE CHECK THAT RUNS RIGHT AFTER IT DECIDES. Build 20, signed by m11, was confirmed against the roster two
/// days ago, and the app was quit since. The launch's swap waits (gate 4d); the
/// check re-reads the unchanged head's roster because the stage owes it, and:
///
/// * a roster that still trusts m11 CONFIRMS the stage — its stamp moves to the
///   check's time, the launch's booked wait is withdrawn, and the same gates now
///   admit it, for the in-session lane or the next launch to apply;
/// * a roster that revokes m11 RETIRES it, unconfirmed;
/// * a check that admitted no roster (a pointer read, a failed fetch) confirms
///   nothing — the negative control.
#[test]
fn the_check_after_a_launch_confirms_a_stale_stage_or_retires_a_revoked_one() {
    let now = crate::install::unix_now_secs();
    let two_days_ago = now - 2 * crate::manifest::ROSTER_FRESHNESS_SECS;
    let stamp = aterm_types::rfc3339::format_rfc3339(now);
    let observe = |staging: &Staging, roster_seq: u64, revoked: &[String]| {
        observe_floor_and_withdraw_at(
            staging,
            10,
            crate::manifest::Floor::read(&staging.floor()),
            &crate::manifest::FloorObservation {
                roster_seq,
                revoked,
                ..crate::manifest::FloorObservation::default()
            },
            &stamp,
        )
        .unwrap()
    };

    // Still trusted: confirmed.
    let staging = Staging::scratch("roster-reconfirm");
    let stale = publish_stage_confirmed_at(&staging, "m11", two_days_ago);
    let Some(crate::ApplyOutcome::Deferred(wait)) =
        crate::install::stage_admission_gates(&staging, 10, &stale, now)
    else {
        panic!("PRECONDITION: the cold launch waits");
    };
    crate::health::Health::record_apply_refusal(&staging.health(), 10, &wait);
    assert!(
        stage_owes_roster_reconfirmation(&staging, 10, now, true),
        "the check re-reads an unchanged head's roster for it"
    );
    // A check that admitted no roster confirms nothing.
    observe(&staging, 0, &[]);
    assert_eq!(
        Ready::read(&staging.ready).unwrap().roster_confirmed_at,
        stale.roster_confirmed_at
    );
    observe(&staging, 9, &[]);
    let confirmed = Ready::read_publishable(&staging).expect("kept and published");
    assert_eq!(
        confirmed.roster_confirmed_at.as_deref(),
        Some(stamp.as_str())
    );
    assert_eq!(
        crate::install::stage_admission_gates(&staging, 10, &confirmed, now),
        None,
        "confirmed, the stage swaps"
    );
    assert!(!stage_owes_roster_reconfirmation(&staging, 10, now, true));
    assert!(
        crate::health::Health::read(&staging.health())
            .last_apply_refusal
            .is_empty(),
        "the launch's wait is answered"
    );
    assert_eq!(
        crate::install::handoff_cached_pass_refusal_at(
            &staging,
            20,
            "0123456789abcdef0123456789abcdef01234567"
        ),
        None,
        "and the in-session lane may hand off to it"
    );
    let _ = std::fs::remove_dir_all(&staging.root);

    // Revoked in a roster no check had read: retired, never confirmed.
    let staging = Staging::scratch("roster-reconfirm-revoked");
    let stale = publish_stage_confirmed_at(&staging, "m11", two_days_ago);
    assert!(matches!(
        crate::install::stage_admission_gates(&staging, 10, &stale, now),
        Some(crate::ApplyOutcome::Deferred(_))
    ));
    observe(&staging, 10, &["m11".to_string()]);
    assert!(!staging.ready.exists(), "the revoked stage is retired");
    assert!(Ready::read_published_bundle(&staging).is_none());
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A STAGE NO CHECK CAN CONFIRM IS SAID ON THE STATUS LINE. Gate 4d's wait promises the stage "installs once the next
/// update check confirms it", but only a check that admits the channel head's
/// roster confirms anything: a head that is not an app release reads no roster,
/// and a build whose anchor cannot verify the roster (a master rotation it
/// predates) never will. Such a stage waits as long as the channel state lasts, so the check's line names the
/// state and the wait. CONTROLS: a stage confirmed inside the day, a stage the
/// running build has passed, and a build with no roster tier say nothing.
#[test]
fn a_stage_no_check_can_confirm_is_said_on_the_status_line() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::status::clear_check_note();
    let now = crate::install::unix_now_secs();
    let staging = Staging::scratch("unconfirmable-stage");
    publish_stage_confirmed_at(
        &staging,
        "m3",
        now - 2 * crate::manifest::ROSTER_FRESHNESS_SECS,
    );
    // A head that is not an app release: said, with the stage's wait.
    record_head_without_app(&staging, 10, "atpkg-index-44");
    let text = std::fs::read_to_string(&staging.status).unwrap();
    assert!(
        text.contains("channel head atpkg-index-44 has no app manifest yet")
            && text.contains(
                "staged build 20 is not installed: it waits to be confirmed against a \
                 current machine roster (last at "
            )
            && text.contains("once the channel head is an app release again"),
        "{text}"
    );
    // The stale anchor's words, as the manifest-class branch appends them.
    let anchor = unconfirmable_stage_note(&staging, 10, now, true, STALE_ANCHOR_STAGE_WAIT)
        .expect("a stale stage waits");
    assert!(
        anchor.ends_with("and this build can never confirm it"),
        "{anchor}"
    );
    // A stage never confirmed on this Mac says so.
    let mut never = Ready::read(&staging.ready).unwrap();
    never.roster_confirmed_at = None;
    std::fs::write(&staging.ready, never.to_toml().unwrap()).unwrap();
    assert!(
        unconfirmable_stage_note(&staging, 10, now, true, "x")
            .is_some_and(|note| note.contains("(never on this Mac)"))
    );

    // CONTROLS.
    assert_eq!(
        unconfirmable_stage_note(&staging, 20, now, true, "x"),
        None,
        "a stage the running build has passed"
    );
    assert_eq!(
        unconfirmable_stage_note(&staging, 10, now, false, "x"),
        None,
        "a build with no roster tier"
    );
    publish_stage_confirmed_at(&staging, "m3", now - 600);
    assert_eq!(
        unconfirmable_stage_note(&staging, 10, now, true, "x"),
        None,
        "confirmed inside the day, the stage installs"
    );
    record_head_without_app(&staging, 10, "atpkg-index-44");
    let text = std::fs::read_to_string(&staging.status).unwrap();
    assert!(!text.contains("is not installed"), "{text}");
    let _ = std::fs::remove_dir_all(&staging.root);

    // Wired where the stale anchor is said: the manifest-class line appends it.
    let src = include_str!("github.rs");
    let check = &src[src.find("fn check_and_stage_inner(").expect("the check")..];
    let branch = &check[check
        .find("} else if manifest_rejected {")
        .expect("the manifest-class branch")..];
    let branch = &branch[..branch.find("crate::status::record(").expect("its record")];
    assert!(
        branch.contains("let stage_note = is_stale_anchor_refusal(reason)")
            && branch.contains("unconfirmable_stage_note(")
            && branch.contains("line + &stage_note"),
        "{branch}"
    );
}

/// THE STEADY STATE YIELDS TO A DUE CONFIRMATION. An
/// unchanged pointer ends the check after one HEAD — unless the build staged here
/// owes its roster confirmation, when the head is judged again (roster and all)
/// so the floor's observation can confirm or retire it — from an hour on, so the
/// stamp a quit leaves is never much older than that (at half a day, an evening's
/// quit would make the next morning's launch wait). A stage confirmed
/// inside the hour keeps the one-HEAD steady state; so does a stage that is not
/// newer than the running build.
#[test]
fn an_unchanged_head_is_judged_again_when_the_stage_owes_its_roster_confirmation() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("steady-reconfirm");
    let now = crate::install::unix_now_secs();
    publish_stage_confirmed_at(
        &staging,
        "m3",
        now - crate::manifest::ROSTER_RECONFIRM_AFTER_SECS,
    );
    let (outcome, heads) = acquire_web(&staging, Some(WEB_TAG), || redirect_to(WEB_TAG));
    assert_eq!(heads, vec![evergreen_url()]);
    let Ok(Acquisition::Proceed(candidate)) = outcome else {
        panic!("a due confirmation re-judges the unchanged head: {outcome:?}");
    };
    assert_eq!(candidate.release.tag_name, WEB_TAG);
    // CONTROL: confirmed ten minutes ago, the steady state stands.
    publish_stage_confirmed_at(&staging, "m3", now - 600);
    let (outcome, heads) = acquire_web(&staging, Some(WEB_TAG), || redirect_to(WEB_TAG));
    assert_eq!(heads.len(), 1);
    assert!(
        matches!(outcome, Ok(Acquisition::UpToDate { .. })),
        "{outcome:?}"
    );
    // A stage the running build has passed owes nothing.
    assert!(!stage_owes_roster_reconfirmation(
        &staging,
        20,
        now + 86_400 * 2,
        true
    ));
    // Nor does any stage on a build with no roster tier.
    assert!(!stage_owes_roster_reconfirmation(
        &staging,
        10,
        now + 86_400 * 2,
        false
    ));
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE CHECK DATES WHAT IT CONFIRMS AND WHAT IT STAGES BY ITS ROSTER READ. The
/// instant is taken once, before the fetch that reads
/// the roster, and handed to the floor's observation (which confirms a stage
/// already published) and to both container lanes (which publish a new one,
/// after a download that may take hours); no stamp in the check is taken later.
/// Asserted on source: the check needs the network.
#[test]
fn the_check_dates_its_confirmations_by_the_roster_read() {
    let src = include_str!("github.rs");
    let shipped = &src[..src
        .find("#[cfg(test)]\n#[path = \"github_tests.rs\"]\nmod tests;")
        .expect("the test module")];
    let check = &shipped[shipped
        .find("fn check_and_stage_inner(")
        .expect("the check")..];
    let check = &check[..check.find("\n}\n").expect("end of the check")];
    let read = check
        .find(concat!(
            "let roster_read_at = crate::install::now_rfc3339",
            "();"
        ))
        .expect("the read's instant, taken once");
    let fetch = check
        .find(concat!("fetch_authoritative_release", "("))
        .expect("the fetch that reads the roster");
    assert!(read < fetch, "taken before the roster is fetched");
    assert_eq!(check.matches("&roster_read_at").count(), 3);
    for call in [
        concat!("observe_floor_and_withdraw_at", "("),
        concat!("install::stage_from_zip", "("),
        concat!("install::stage_from_dmg", "("),
    ] {
        let at = check.find(call).expect(call);
        let rest = &check[at..];
        let close = [rest.find("\n    )"), rest.find("\n        )")]
            .into_iter()
            .flatten()
            .min()
            .expect("the call's end");
        assert!(
            rest[..close].contains("&roster_read_at"),
            "{call} is handed the roster read"
        );
    }
    assert!(
        !shipped.contains(concat!("observe_floor_and_withdraw", "(")),
        "no shipped caller stamps a confirmation with the time of its write"
    );
}

/// Tier-1 for `NativeUpdateRosterFreshness`. Every reachable
/// state of the model with a stage on disk is laid out in a real staging root —
/// build 20 by m11, its marker stamped at the state's confirmation tick (a tick
/// is half the window of the real clock), the floor at the generation that
/// authorized it; one root per distinct thing the real code reads — and the real
/// code is asked:
///
/// * the swap's gates ([`crate::install::stage_admission_gates`], at the state's
///   tick) admit the stage exactly when the model's `Swap` is enabled, and the
///   swap they admit is a `Swap` step;
/// * the check's observation ([`observe_floor_and_withdraw_at`], under a roster
///   that revokes m11 once the owner has) leaves the disk in a state the model's
///   `Check` reaches.
///
/// NEGATIVE CONTROLS, in real code: the floor-alone lane (the floor's
/// withdrawals and the published stage, nothing else) admits a swap the model
/// refuses — the `Buggy = 1` `SwapOnTheFloorAlone` step into a state
/// `NoSwapPastTheWindow` refutes; and a confirmation stamped without the roster
/// ([`crate::install::confirm_roster_authorization`] on a revoked signer's stage)
/// is `ConfirmWithoutTheRoster`, after which the real gates admit a late swap.
#[test]
fn roster_freshness_conformance() {
    use aterm_spec::interp::State;
    let model = aterm_spec::derive::native_update_roster_freshness_model();
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .expect("a model constant")
    };
    let window = constant("F");
    // A tick is the window over `F`: the model's clock is the gates' clock, and
    // how often a running check re-reads the roster is the environment's
    // (`Check` may fire at any tick), not a tick of its own.
    let tick = crate::manifest::ROSTER_FRESHNESS_SECS / u64::try_from(window).unwrap();
    assert_eq!(
        u64::try_from(window).unwrap() * tick,
        crate::manifest::ROSTER_FRESHNESS_SECS,
        "the window is a whole number of ticks"
    );
    let base: u64 = 1_790_000_000;
    let at = |t: i64| base + u64::try_from(t).unwrap() * tick;
    let tick_of = |stamp: &str| {
        let secs = u64::try_from(aterm_types::rfc3339::parse_utc(stamp).unwrap()).unwrap();
        i64::try_from((secs - base) / tick).unwrap()
    };
    // The stage on disk, build 20 by m11, its marker confirmed at tick `conf`.
    let lay_out = |conf: i64| {
        let staging = Staging::scratch("roster-freshness-t1");
        let ready = publish_stage_confirmed_at(&staging, "m11", at(conf));
        crate::manifest::Floor::observe_and_write(
            &staging.floor(),
            &crate::manifest::FloorObservation {
                roster_seq: 9,
                ..crate::manifest::FloorObservation::default()
            },
        );
        (staging, ready)
    };
    // What the projection reads of the disk: whether the stage is still published,
    // and the tick its marker was last confirmed at.
    let read_disk = |staging: &Staging| {
        Ready::read_published_bundle(staging)
            .map(|ready| ready.roster_confirmed_at.as_deref().map(tick_of))
    };
    let project = |disk: Option<Option<i64>>, state: &State, swapped: Option<bool>| {
        crate::roster_freshness_projection(
            state["now"],
            disk.flatten().unwrap_or(state["conf"]),
            (state["revoked"] == 1).then_some(state["rev_at"]),
            disk.is_some(),
            swapped,
        )
    };
    let late = |state: &State| state["revoked"] == 1 && state["now"] - state["rev_at"] >= window;

    let mut reachable = std::collections::BTreeSet::new();
    let mut frontier = vec![model.init_state()];
    while let Some(state) = frontier.pop() {
        if reachable.insert(state.clone()) {
            for action in &model.actions {
                frontier.extend(model.successors(action.name, &state));
            }
        }
    }
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    // EACH REAL RUN IS KEYED ON EXACTLY WHAT IT READS, and run once per key. The
    // swap reads the stage laid out at `conf` and the clock at `now`; the check
    // reads that stage, whether the roster it admits revokes m11, and `now` (the
    // instant it read the roster at). Neither reads `rev_at`, which only the
    // projection of the state does, so every state that shares a key gets the
    // same answer from the same real calls on a fresh layout, and each state's
    // assertions below are still made against it. Measured: keyed, the 77 states
    // lay out 21 swap and 42 check roots instead of 154; the cost is the volume's
    // (every mkdir waits behind neighbours' flushes), ~2.5x under fsync load.
    let mut swap_runs = std::collections::BTreeMap::new();
    let mut check_runs = std::collections::BTreeMap::new();
    let (mut swaps, mut checks, mut floor_alone_late) = (0, 0, None);
    for state in reachable
        .iter()
        .filter(|state| state["staged"] == 1 && state["swapped"] == 0)
    {
        // The swap: whether the gates admit the stage, the disk after them, and
        // whether the floor alone would have.
        let (admitted, disk, floor_alone) = *swap_runs
            .entry((state["conf"], state["now"]))
            .or_insert_with(|| {
                let (staging, ready) = lay_out(state["conf"]);
                let admitted =
                    crate::install::stage_admission_gates(&staging, 10, &ready, at(state["now"]))
                        .is_none();
                // NEGATIVE CONTROL: the floor-alone lane.
                let floor_alone = crate::manifest::Floor::read(&staging.floor())
                    .withdraws(ready.build_number, ready.machine_id.as_deref())
                    .is_none()
                    && ready.is_publishable(&staging);
                (admitted, read_disk(&staging), floor_alone)
            });
        assert_eq!(
            admitted,
            model.action_enabled("Swap", state),
            "the gates and the model disagree at {state:?}"
        );
        if admitted {
            let next = project(disk, state, Some(late(state)));
            assert!(
                model.successors("Swap", state).contains(&next),
                "{state:?} -> {next:?}"
            );
            swaps += 1;
        }
        if floor_alone && !admitted && late(state) {
            let next = project(disk, state, Some(true));
            assert!(
                buggy
                    .successors("SwapOnTheFloorAlone", state)
                    .contains(&next)
            );
            floor_alone_late = Some(next);
        }

        // The check.
        let revoked = state["revoked"] == 1;
        let disk = *check_runs
            .entry((state["conf"], revoked, state["now"]))
            .or_insert_with(|| {
                let (staging, _) = lay_out(state["conf"]);
                let revocations: Vec<String> = if revoked {
                    vec!["m11".to_string()]
                } else {
                    Vec::new()
                };
                observe_floor_and_withdraw_at(
                    &staging,
                    10,
                    crate::manifest::Floor::read(&staging.floor()),
                    &crate::manifest::FloorObservation {
                        roster_seq: 9 + u64::from(revoked),
                        revoked: &revocations,
                        ..crate::manifest::FloorObservation::default()
                    },
                    &aterm_types::rfc3339::format_rfc3339(at(state["now"])),
                )
                .unwrap();
                read_disk(&staging)
            });
        let next = project(disk, state, None);
        assert!(
            model.successors("Check", state).contains(&next),
            "the check's disk is no Check step: {state:?} -> {next:?}"
        );
        checks += 1;
    }
    assert!(swaps > 0 && checks > 0, "{swaps} swaps, {checks} checks");
    let late_swap = floor_alone_late.expect("the floor-alone lane swaps late somewhere");
    assert!(!model.check_invariant("NoSwapPastTheWindow", &late_swap));

    // NEGATIVE CONTROL: a confirmation stamped without the roster. The owner
    // revoked m11 at tick 0, and two ticks on, a stamp written blind...
    let mut state = model.init_state();
    for action in ["Revoke", "Tick", "Tick"] {
        assert!(model.fire(action, &mut state));
    }
    let (staging, ready) = lay_out(state["conf"]);
    crate::install::confirm_roster_authorization(
        &staging,
        &ready,
        &aterm_types::rfc3339::format_rfc3339(at(state["now"])),
    )
    .unwrap();
    let blind = project(read_disk(&staging), &state, None);
    assert!(
        buggy
            .successors("ConfirmWithoutTheRoster", &state)
            .contains(&blind)
    );
    assert!(
        !model.successors("Check", &state).contains(&blind),
        "a check that read the roster retires it instead"
    );
    // ...and the real gates then admit a swap the window should have stopped.
    let stamped = Ready::read_published_bundle(&staging).unwrap();
    assert_eq!(
        crate::install::stage_admission_gates(&staging, 10, &stamped, at(blind["now"])),
        None
    );
    let swapped = project(read_disk(&staging), &blind, Some(late(&blind)));
    assert!(!model.check_invariant("NoSwapPastTheWindow", &swapped));
}

/// A STAGE THE FLOOR WITHDRAWS IS ANNOUNCED NOWHERE, AND IS RETIRED BY THE
/// CHECK THAT SAW THE WITHDRAWAL. Build 20 is staged; the owner yanks it
/// (`min_build = 21`). The floor rises on observation, and neither the check's
/// `Some` answer (which arms the apply lane), its backoff line nor `update status`
/// offers build 20 again.
#[test]
fn a_yanked_or_revoked_stage_is_announced_nowhere_and_retired_by_the_check() {
    let staging = Staging::scratch("floor-withdraws-stage");
    publish_attributed_stage(&staging, 20, "m3");
    let mut manifest = candidate_manifest();
    manifest.build_number = 30;
    let now = 1_000_000;
    crate::manifest::FailedMark::record_stage_failure(
        &staging.failed(),
        manifest.build_number,
        &manifest.sha256,
        now,
    );
    // NEGATIVE CONTROL: with no floor the stage is offered everywhere.
    assert_eq!(applicable_stage(&staging, 10), Some("0.0.20".to_string()));
    let backoff = stage_backoff(&staging, &manifest, 10, now, Asked::Schedule).expect("suppressed");
    assert!(backoff.applicable.is_some());

    // THE YANK, observed but not yet acted on.
    crate::manifest::Floor::observe_and_write(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            min_build: 21,
            machine: Some("m3"),
            ..crate::manifest::FloorObservation::default()
        },
    );
    assert_eq!(applicable_stage(&staging, 10), None, "no apply lane arms");
    let backoff = stage_backoff(&staging, &manifest, 10, now, Asked::Schedule).expect("suppressed");
    assert!(
        backoff.applicable.is_none(),
        "{}",
        backoff.status_line(manifest.build_number)
    );
    assert!(
        Ready::read_publishable(&staging).is_none(),
        "status says none"
    );
    // The check that observes it retires the stage, under the apply lock.
    let floor = crate::manifest::Floor::read(&staging.floor());
    observe_floor_and_withdraw(
        &staging,
        10,
        floor,
        &crate::manifest::FloorObservation::default(),
    )
    .unwrap();
    assert!(Ready::read_published_bundle(&staging).is_none(), "retired");
    let status = std::fs::read_to_string(&staging.status).unwrap_or_default();
    assert!(status.contains("yanked"), "{status}");

    // A REVOKED SIGNER, the same way — and only that signer's stage.
    let staging = Staging::scratch("floor-revokes-stage");
    publish_attributed_stage(&staging, 20, "m3");
    let revoked = vec!["m11".to_string()];
    let floor = crate::manifest::Floor::read(&staging.floor());
    observe_floor_and_withdraw(
        &staging,
        10,
        floor,
        &crate::manifest::FloorObservation {
            roster_seq: 10,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    )
    .unwrap();
    assert!(
        Ready::read_publishable(&staging).is_some(),
        "m3's stage is untouched by m11's revocation"
    );
    publish_attributed_stage(&staging, 20, "m11");
    assert_eq!(
        applicable_stage(&staging, 10),
        None,
        "a stage by a machine the floor has seen revoked is offered nowhere"
    );
    let floor = crate::manifest::Floor::read(&staging.floor());
    observe_floor_and_withdraw(
        &staging,
        10,
        floor,
        &crate::manifest::FloorObservation::default(),
    )
    .unwrap();
    assert!(Ready::read_published_bundle(&staging).is_none(), "retired");
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A FLOOR THE CHECK COULD NOT READ BACK IS NOT JUDGED AS ZERO. The check ends as a passing refusal instead of going on with
/// `min_build`, `high_water` and `roster_seq` all read as 0 — and the roster in
/// hand still retires the stage of a machine it revokes.
#[test]
fn a_floor_the_check_could_not_read_back_ends_the_check_as_a_moment() {
    use std::os::unix::fs::PermissionsExt;
    let staging = Staging::scratch("floor-unreadable-check");
    publish_attributed_stage(&staging, 20, "m11");
    std::fs::write(staging.floor(), "min_build = 21\nroster_seq = 10\n").unwrap();
    let floor = crate::manifest::Floor::read(&staging.floor());
    std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let plain = observe_floor_and_withdraw(
        &staging,
        10,
        floor.clone(),
        &crate::manifest::FloorObservation::default(),
    );
    assert!(
        Ready::read_published_bundle(&staging).is_some(),
        "nothing it could not read retires a stage"
    );
    let revoked = vec!["m11".to_string()];
    let revoking = observe_floor_and_withdraw(
        &staging,
        10,
        floor,
        &crate::manifest::FloorObservation {
            roster_seq: 11,
            revoked: &revoked,
            ..crate::manifest::FloorObservation::default()
        },
    );
    std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(0o600)).unwrap();
    for result in [&plain, &revoking] {
        assert!(
            result
                .as_ref()
                .is_err_and(|why| crate::is_passing_refusal(why) && why.contains("floor")),
            "{result:?}"
        );
    }
    assert!(
        Ready::read_published_bundle(&staging).is_none(),
        "the roster in hand still retires its revoked signer's stage"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A FLOOR THAT STAYS UNREADABLE IS SAID, NOT ONLY REFUSED. The check fails
/// closed on a floor it could not read — right for a moment — but a floor that
/// stays so (root-owned after a `sudo` run) would stop updates for good. Each such
/// check is booked, and at `PERSISTENT_AFTER` the persistent notice names the file
/// and the remedy.
///
/// CONTROLS: the same floor readable again is read, and books nothing; an absent
/// floor is the first-contact default, and books nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_floor_that_stays_unreadable_is_booked_until_it_is_said() {
    use std::os::unix::fs::PermissionsExt as _;
    let staging = Staging::scratch("floor-unreadable");
    let absent = floor_snapshot(&staging).expect("no floor is the first-contact default");
    assert_eq!(absent.min_build, 0);
    std::fs::write(staging.floor(), "min_build = 500\nroster_seq = 10\n").unwrap();
    std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads a mode-000 file, and then the premise does not hold.
    let premise = std::fs::read(staging.floor()).is_err();
    let refusals: Vec<_> = (0..crate::PERSISTENT_AFTER)
        .map(|_| floor_snapshot(&staging))
        .collect();
    std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(0o600)).unwrap();
    if premise {
        let floor = staging.floor().display().to_string();
        for refusal in &refusals {
            let refusal = refusal
                .as_ref()
                .expect_err("an unreadable floor ends the check");
            assert!(crate::is_passing_refusal(refusal), "{refusal}");
            assert!(refusal.contains(&floor), "{refusal}");
        }
        // The streak spans the persistence window (`Health::persistent_class`).
        crate::health::backdate_streaks(&staging.health(), crate::PERSISTENT_SPAN);
        let h = crate::health::Health::read(&staging.health());
        assert_eq!(
            h.persistent_class(),
            Some(("pipeline", crate::PERSISTENT_AFTER)),
            "booked, so it can escalate: {h:?}"
        );
        let (_, body) = crate::persistent_failure_notice("pipeline", h.pipeline_failures, &h, 1);
        assert!(
            body.contains(&floor)
                && body.contains("readable again")
                && body.contains("remove it")
                && !body.contains("pipeline is likely broken")
                && !body.contains(crate::PASSING_REFUSAL_KEY),
            "the notice names the file and the remedy: {body}"
        );
    }
    let before = crate::health::Health::read(&staging.health()).pipeline_failures;
    let readable = floor_snapshot(&staging).expect("control: readable again");
    assert_eq!((readable.min_build, readable.roster_seq), (500, 10));
    assert_eq!(
        crate::health::Health::read(&staging.health()).pipeline_failures,
        before,
        "control: a floor that was read books nothing"
    );
}

#[test]
fn a_release_is_held_when_its_roster_generation_was_superseded_mid_check() {
    // Quiescent: our own write put the floor at our generation.
    assert!(!roster_authority_superseded(Some(9), 9));
    // Raced: a concurrent instance recorded 10 — the release's authority is stale.
    assert!(roster_authority_superseded(Some(9), 10));
    // Inert tier: nothing was observed, nothing can be superseded.
    assert!(!roster_authority_superseded(None, u64::MAX));
}

/// A STRANDED CLIENT IS TOLD TO REINSTALL, NOT TO WAIT FOR THE PUBLISHER.
///
/// One refusal is permanent for the BUILD rather than for the channel: the pinned
/// paper master cannot verify the roster (a master rotation this build predates).
/// The head is the one candidate and nothing falls back, so retrying changes nothing
/// and no release the publisher cuts will ever verify here. The reason travels
/// ([`AuthoritativeFetch::rejection_reason`]) and the anchor arm carries
/// [`STALE_ANCHOR_KEY`] and the reinstall remedy. The OTHER roster refusals (a lapsed
/// or rolled-back generation) are the publisher's to fix and must NOT carry the key —
/// a reinstall would change nothing for them.
#[test]
fn a_stale_anchor_names_the_reinstall_and_a_publisher_fault_does_not() {
    // (A) A roster signed by a master this build does not pin.
    let good = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        4,
        &MASTER_SEED_FIXTURE,
        4,
    );
    let masters = [good.master_pub.as_str()];
    let wrong_master = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        4,
        &OTHER_MASTER_FIXTURE,
        4,
    );
    let (forged, _) = run_chain(&wrong_master, &masters, 0, ROSTER_NOW);
    let reason = forged.rejection_reason.as_deref().expect("reason");
    assert!(
        is_stale_anchor_refusal(reason) && reason.contains(STALE_ANCHOR_REMEDY),
        "an unverifiable roster is the stranded client's arm: {reason}"
    );
    assert!(
        reason.contains("v0.10.0"),
        "the reason still names the release it refused: {reason}"
    );

    // (B) The publisher's faults keep the publisher wording: a lapsed roster and a
    //     rolled-back one are refused with a reason that carries NO reinstall.
    let (lapsed, _) = run_chain(&good, &masters, 0, 1_900_000_000);
    let reason = lapsed.rejection_reason.as_deref().expect("reason");
    assert!(
        lapsed.manifest_rejected && !is_stale_anchor_refusal(reason),
        "a lapsed roster is the publisher's to refresh, not a reinstall: {reason}"
    );
    let (replayed, _) = run_chain(&good, &masters, 5, ROSTER_NOW);
    let reason = replayed.rejection_reason.as_deref().expect("reason");
    assert!(
        replayed.manifest_rejected && !is_stale_anchor_refusal(reason),
        "a rolled-back roster is a replay, not a stale anchor: {reason}"
    );
    // And a verified, accepted release carries no reason at all.
    let (accepted, _) = run_chain(&good, &masters, 0, ROSTER_NOW);
    assert!(accepted.selected.is_some() && accepted.rejection_reason.is_none());
}

/// A SIGNATURE DOWNLOAD THAT IS NOT A SIGNATURE is a delivery fault, not a verdict on
/// the anchor. A TLS-inspecting proxy that answers the asset host with HTTP 200 and
/// a small block page hands `curl -f` a body it calls success, and a reinstall
/// would change nothing. A body that cannot be an Ed25519 signature (not 64
/// bytes) has judged nothing, so it is the transport arm — still a refusal,
/// nothing selected. A well-formed signature that fails to verify stays the
/// stale-anchor verdict (the test above).
#[test]
fn a_signature_that_is_not_signature_shaped_is_a_delivery_fault_not_a_stale_anchor() {
    let base = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        4,
        &MASTER_SEED_FIXTURE,
        4,
    );
    let masters = [base.master_pub.as_str()];
    let page = b"<html><body>Blocked by policy</body></html>".to_vec();
    let roster_sig_blocked = Chain {
        roster_sig: page.clone(),
        ..base.clone()
    };
    let appcast_sig_blocked = Chain {
        manifest_sig: page,
        ..base.clone()
    };
    for (what, c) in [
        ("roster signature", &roster_sig_blocked),
        ("appcast signature", &appcast_sig_blocked),
    ] {
        let (fetched, _) = run_chain(c, &masters, 0, ROSTER_NOW);
        let reason = fetched.rejection_reason.clone().unwrap_or_default();
        assert!(
            fetched.selected.is_none(),
            "a {what} block page still refuses the release"
        );
        assert!(
            !is_stale_anchor_refusal(&reason),
            "a {what} that is not 64 bytes is a delivery fault, not a stale anchor: \
             {reason}"
        );
        assert!(
            fetched.appcast_fetch_error && !fetched.manifest_rejected,
            "a {what} block page is booked as a delivery failure, not a manifest verdict"
        );
    }
}

/// A ROSTER ASSET THAT WILL NOT DOWNLOAD is a TRANSPORT failure, not a publisher
/// error — and it still refuses.
///
/// The distinction is what the operator is told. `manifest_rejected` escalates to
/// "this Mac cannot install any release until that is fixed at the publisher", which
/// is a false accusation for a flaky network — and now that the roster is the sole
/// authority, every armed client's fetch of it is on that path.
///
/// It is also the ATTACKER'S branch: dropping or stalling the `aterm-machines.toml`
/// fetch must never produce anything but a refusal. The closure below serves a good
/// appcast and a good appcast signature, so the only thing missing is the roster.
#[test]
fn a_roster_that_cannot_be_fetched_is_reported_as_transport_and_still_refuses() {
    let c = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        4,
        &MASTER_SEED_FIXTURE,
        4,
    );
    // Control: the same chain with its roster served is accepted, so the refusal
    // below is about the missing roster and nothing else.
    let masters_control = [c.master_pub.as_str()];
    let (control, _) = run_chain(&c, &masters_control, 0, ROSTER_NOW);
    assert!(
        control.selected.is_some(),
        "precondition: the served chain passes"
    );
    let masters = [c.master_pub.as_str()];
    let selected = candidate_for(vec![release_with_roster("v0.10.0")])
        .unwrap()
        .unwrap();
    let mut download = |url: &str, _max: u64| match url {
        "m-url" => Ok(c.manifest.clone()),
        // Served, deliberately. See the doc comment.
        "sig-url" => Ok(c.manifest_sig.clone()),
        "roster-url" => Err("connection reset".to_string()),
        other => Err(format!("unexpected fetch {other}")),
    };
    let fetched = fetch_authoritative_release(
        Some(selected),
        &mut download,
        &RosterPolicy {
            master_pubkeys: &masters,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(fetched.selected.is_none(), "no roster, no release");
    assert!(
        fetched.attribution.is_none(),
        "nothing may be accepted, attributed or not"
    );
    assert!(
        fetched.appcast_fetch_error,
        "a fetch failure is a pipeline-class failure"
    );
    assert!(
        !fetched.manifest_rejected,
        "a network failure must not accuse the publisher of shipping a bad release"
    );

    // THE SAME SUPPRESSION, ONE ASSET OVER. The roster body arrives and its master
    // signature does not — the other half an attacker can withhold independently, and
    // a second place a fallback could be bolted on.
    let selected = candidate_for(vec![release_with_roster("v0.10.0")])
        .unwrap()
        .unwrap();
    let mut download = |url: &str, _max: u64| match url {
        "m-url" => Ok(c.manifest.clone()),
        "sig-url" => Ok(c.manifest_sig.clone()),
        "roster-url" => Ok(c.roster.clone()),
        "roster-sig-url" => Err("connection reset".to_string()),
        other => Err(format!("unexpected fetch {other}")),
    };
    let fetched = fetch_authoritative_release(
        Some(selected),
        &mut download,
        &RosterPolicy {
            master_pubkeys: &masters,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(
        fetched.selected.is_none() && fetched.attribution.is_none(),
        "a roster with no master signature authorizes nothing"
    );
    assert!(fetched.appcast_fetch_error);
    assert!(!fetched.manifest_rejected);
}

/// THE RATCHET AND THE BIND, on the transport path, under the new authority.
///
/// `roster_seq` appears in two documents: the roster's own generation, and the copy
/// inside the signed appcast. The appcast may not claim a generation NEWER than the
/// roster that authorized it — that is what stops an old roster being paired with a
/// new release. It MAY claim an older one: the roster travels as an
/// asset on the channel head, and a join attaches the new pair to releases attributed
/// under the previous generation — the steady state of a multi-machine channel, and
/// verifying under a newer roster is strictly stronger. And the accepted generation
/// must reach the caller, or the durable floor never advances and the replay defence
/// is inert.
#[test]
fn the_roster_generation_must_agree_between_the_roster_and_the_signed_manifest() {
    // The roster is at generation 6; the appcast claims 7 — inside its own signed
    // bytes, so this is a genuine signature over a claim the roster cannot back:
    // an OLD roster presented with a NEWER release.
    let lying = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        6,
        &MASTER_SEED_FIXTURE,
        7,
    );
    let masters = [lying.master_pub.as_str()];
    let (refused, _) = run_chain(&lying, &masters, 0, ROSTER_NOW);
    assert!(
        refused.selected.is_none() && refused.manifest_rejected,
        "an appcast may not name a roster generation newer than the one that \
         authorized it"
    );
    // The roster is at generation 6; the appcast was attributed under 5 — a release
    // published before a join, now carrying the newer pair: ADMITTED, and the floor
    // ratchets to the roster's generation (6), not the manifest's.
    let redressed = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        6,
        &MASTER_SEED_FIXTURE,
        5,
    );
    let (admitted, _) = run_chain(&redressed, &masters, 0, ROSTER_NOW);
    assert!(
        admitted.selected.is_some(),
        "a newer roster paired with an older release is the post-join steady state"
    );
    assert_eq!(admitted.observed_roster_seq, Some(6));

    // Truthful, same everything else: accepted, and the generation is handed back for
    // the durable floor to ratchet.
    let honest = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        6,
        &MASTER_SEED_FIXTURE,
        6,
    );
    let (ok, _) = run_chain(&honest, &masters, 0, ROSTER_NOW);
    assert!(ok.selected.is_some());
    assert_eq!(ok.observed_roster_seq, Some(6));
    // ...and the floor it just advanced past now refuses that same generation's
    // predecessor forever.
    let older = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        5,
        &MASTER_SEED_FIXTURE,
        5,
    );
    let (rolled, _) = run_chain(&older, &masters, 6, ROSTER_NOW);
    assert!(rolled.selected.is_none() && rolled.manifest_rejected);
}

// ---------------------------------------------------------------------------
// THE ONE LANE, MEASURED.
//
// The pointer's HEAD transport is injected and the asset downloader is a closure,
// so every request a check makes is COUNTED here rather than claimed: the
// pointer's failure paths make exactly their one HEAD, and the downloads the
// fixtures make are asserted off `api.github.com` (`cdn::is_api_host`). "Zero
// `api.github.com` requests" is then a fact the test runner enforces. (There is no
// second lane to fake: the metered API lane, and the cadence it ran on, are
// retired — every check is this one, on the one cadence.)
// ---------------------------------------------------------------------------

/// The release the fixture channel's pointer names.
const WEB_TAG: &str = "v0.10.0";

/// The DERIVED tag-specific URL of `name` on the fixture channel.
fn tag_url(tag: &str, name: &str) -> String {
    aterm_update_core::cdn::release_download_url("alabsystems", "aterm", tag, name).unwrap()
}

/// The DERIVED tag-specific URL prefix (`…/releases/download/<tag>/`) every GET of
/// `tag` must carry. (The builder refuses an empty name, so it is taken off a
/// placeholder.)
fn tag_prefix(tag: &str) -> String {
    let url = tag_url(tag, "x");
    url[..url.len() - 1].to_string()
}

/// The one evergreen URL a check is allowed to HEAD.
fn evergreen_url() -> String {
    aterm_update_core::pointer::latest_download_url("alabsystems", "aterm", APPCAST_ASSET).unwrap()
}

/// GitHub's measured answer to the evergreen HEAD: a 302 whose `Location` is the
/// tag-specific appcast URL of `tag`.
fn redirect_to(tag: &str) -> Result<HeadAnswer, HttpError> {
    Ok(HeadAnswer {
        code: 302,
        location: Some(tag_url(tag, APPCAST_ASSET)),
    })
}

/// The owner side of one release: a master-signed roster naming m3, and an
/// appcast for [`WEB_TAG`] signed by m3 whose `url` field is `container_url` — the
/// derived DMG URL by default, or whatever a test wants to bind against.
struct WebChannel {
    master_pub: String,
    appcast: Vec<u8>,
    appcast_sig: Vec<u8>,
    roster: Vec<u8>,
    roster_sig: Vec<u8>,
}

fn web_channel(container_url: Option<&str>) -> WebChannel {
    let master = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE).unwrap();
    let m3 = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
    let machine_pub = b64(m3.public_key().as_ref());
    let roster = aterm_update_core::roster::Roster {
        schema: 1,
        roster_seq: 4,
        valid_until: "2027-02-01T00:00:00Z".into(),
        machines: vec![aterm_update_core::roster::Machine {
            id: "m3".into(),
            pubkey: machine_pub,
            added_at: "2026-08-04T00:00:00Z".into(),
            not_after: None,
        }],
        revoked: vec![],
    };
    let roster = roster.to_toml().unwrap().into_bytes();
    let mut appcast = String::from_utf8(manifest_bytes("0.10.0", 10, 0)).unwrap();
    if let Some(url) = container_url {
        appcast.push_str(&format!("url = {url:?}\n"));
    }
    appcast.push_str("machine_id = \"m3\"\nroster_seq = 4\n");
    let appcast = appcast.into_bytes();
    WebChannel {
        master_pub: b64(master.public_key().as_ref()),
        appcast_sig: m3.sign(&appcast).as_ref().to_vec(),
        appcast,
        roster_sig: master.sign(&roster).as_ref().to_vec(),
        roster,
    }
}

impl WebChannel {
    /// The pinned-master policy every armed client runs, over `masters` (the
    /// caller's `[channel.master_pub.as_str()]` — a borrow the policy outlives).
    fn policy<'a>(&self, masters: &'a [&'a str]) -> RosterPolicy<'a> {
        RosterPolicy {
            master_pubkeys: masters,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        }
    }

    /// Serve the four assets of [`WEB_TAG`] by their DERIVED URL — nothing else
    /// exists, exactly like the real download host.
    fn serve(&self, url: &str) -> Result<Vec<u8>, String> {
        let base = tag_prefix(WEB_TAG);
        let Some(name) = url.strip_prefix(base.as_str()) else {
            return Err(format!("HTTP 404: no such asset {url}"));
        };
        match name {
            APPCAST_ASSET => Ok(self.appcast.clone()),
            APPCAST_SIG_ASSET => Ok(self.appcast_sig.clone()),
            aterm_update_core::roster::ROSTER_ASSET => Ok(self.roster.clone()),
            aterm_update_core::roster::ROSTER_SIG_ASSET => Ok(self.roster_sig.clone()),
            _ => Err(format!("HTTP 404: no such asset {url}")),
        }
    }
}

/// The build every fixture check runs as.
const WEB_BUILD: u64 = 5;

/// Run the check's acquisition against a pointer that answers `answer`, counting
/// every HEAD it makes. `known` writes a ledger that authorized that tag AS THIS BUILD,
/// AGAINST THE FIXTURE SOURCE (the only ledger the shortcut may trust). Returns the
/// outcome and every URL the HEAD transport was asked.
fn acquire_web(
    staging: &Staging,
    known: Option<&str>,
    answer: impl FnMut() -> Result<HeadAnswer, HttpError>,
) -> (Result<Acquisition, String>, Vec<String>) {
    if let Some(tag) = known {
        write_ledger(staging, tag, WEB_BUILD, "alabsystems/aterm");
    } else {
        let _ = std::fs::remove_file(&staging.status);
    }
    acquire_web_from(staging, &test_source(), answer)
}

/// A ledger that authorized `tag` while running `build`, against `source`.
fn write_ledger(staging: &Staging, tag: &str, build: u64, source: &str) {
    std::fs::write(
        &staging.status,
        format!(
            "schema = 1\noutcome = \"x\"\ncurrent_build = {build}\nlatest_tag = {tag:?}\n\
             latest_source = {source:?}\nlatest_authorized_build = {build}\n\
             latest_release_build = {build}\n"
        ),
    )
    .unwrap();
}

/// [`acquire_web`] over whatever ledger is on disk, as `source`.
fn acquire_web_from(
    staging: &Staging,
    source: &Source,
    mut answer: impl FnMut() -> Result<HeadAnswer, HttpError>,
) -> (Result<Acquisition, String>, Vec<String>) {
    let mut heads = Vec::new();
    let mut head = |url: &str| {
        heads.push(url.to_string());
        answer()
    };
    let outcome = acquire(staging, WEB_BUILD, source, &mut head);
    (outcome, heads)
}

/// **THE measurement.** A check whose pointer has MOVED costs ONE HEAD of
/// the evergreen URL plus FOUR GETs — appcast, its signature, the roster, its
/// signature — every one addressed by the DERIVED tag-specific URL, none through
/// `latest` again, none to `api.github.com`, none with a credential; the armed
/// roster chain runs on those bytes unchanged and the manifest's `url` binds to
/// the pointer's tag.
#[test]
fn a_web_lane_check_makes_zero_api_requests_and_only_tag_specific_gets() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("zero-api");
    let source = test_source();
    let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
    let masters = [channel.master_pub.as_str()];
    let (outcome, heads) = acquire_web(&staging, None, || redirect_to(WEB_TAG));
    assert_eq!(
        heads,
        vec![evergreen_url()],
        "one HEAD, of the evergreen URL"
    );
    let Ok(Acquisition::Proceed(candidate)) = outcome else {
        panic!("a moved pointer proceeds to acquisition: {outcome:?}");
    };
    assert_eq!(candidate.version, "0.10.0");
    assert_eq!(candidate.release.tag_name, WEB_TAG);
    for asset in &candidate.release.assets {
        assert_eq!(
            asset.url,
            tag_url(WEB_TAG, &asset.name),
            "every asset URL is the derived tag-specific one"
        );
        assert!(!aterm_update_core::cdn::is_api_host(&asset.url));
    }

    let mut gets: Vec<String> = Vec::new();
    let mut download = |url: &str, _max: u64| {
        gets.push(url.to_string());
        channel.serve(url)
    };
    let fetched =
        fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
    let (manifest, release, _) = fetched
        .selected
        .as_ref()
        .expect("the armed chain accepts the rostered release over the download host");
    assert_eq!(
        fetched
            .attribution
            .as_ref()
            .map(|who| who.machine_id.as_str()),
        Some("m3")
    );
    assert_eq!(fetched.observed_roster_seq, Some(4));
    web_container_url_agrees(&source, &release.tag_name, manifest)
        .expect("the manifest's url names the derived container URL under the tag");

    // The request ledger.
    let prefix = tag_prefix(WEB_TAG);
    assert_eq!(
        gets.len(),
        4,
        "appcast, its sig, the roster, its sig: {gets:?}"
    );
    assert_eq!(gets[0], tag_url(WEB_TAG, APPCAST_ASSET));
    for url in &gets {
        assert!(url.starts_with(&prefix), "not tag-specific: {url}");
        assert!(
            !url.contains("/releases/latest/"),
            "through `latest`: {url}"
        );
        assert!(!aterm_update_core::cdn::is_api_host(url), "metered: {url}");
    }
    let mut names: Vec<&str> = gets.iter().map(|u| &u[prefix.len()..]).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            APPCAST_ASSET,
            APPCAST_SIG_ASSET,
            aterm_update_core::roster::ROSTER_ASSET,
            aterm_update_core::roster::ROSTER_SIG_ASSET,
        ]
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE STEADY STATE: a pointer naming the tag this ledger last authorized ends the
/// check after its one HEAD — no GET, no API. Any OTHER recorded tag, or none, is
/// re-judged.
#[test]
fn the_steady_state_is_one_head_when_the_pointer_names_the_authorized_tag() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("steady");
    let (outcome, heads) = acquire_web(&staging, Some(WEB_TAG), || redirect_to(WEB_TAG));
    assert_eq!(heads, vec![evergreen_url()]);
    let Ok(Acquisition::UpToDate { tag }) = outcome else {
        panic!("an unchanged pointer is the steady state: {outcome:?}");
    };
    assert_eq!(tag, WEB_TAG);
    assert_eq!(
        crate::status::latest_tag(&staging, WEB_BUILD, &test_source()).as_deref(),
        Some(WEB_TAG)
    );
    // A pointer that moved past the recorded tag — or a ledger with none — proceeds.
    for known in [Some("v0.9.0"), None] {
        let (outcome, heads) = acquire_web(&staging, known, || redirect_to(WEB_TAG));
        assert_eq!(heads.len(), 1);
        assert!(
            matches!(outcome, Ok(Acquisition::Proceed(_))),
            "{known:?}: {outcome:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn an_unchanged_web_head_refetches_a_signed_release_after_its_stage_is_lost() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("web-lost-stage");
    let source = test_source();
    let mut channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
    // The generic transport fixture omits commit identity. This publication
    // test needs one, covered by the same real detached signature.
    channel
        .appcast
        .extend_from_slice(b"commit = \"0123456789abcdef0123456789abcdef01234567\"\n");
    let signer = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
    channel.appcast_sig = signer.sign(&channel.appcast).as_ref().to_vec();
    let masters = [channel.master_pub.as_str()];
    crate::status::clear_check_note();
    let (outcome, _) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    let Ok(Acquisition::Proceed(candidate)) = outcome else {
        panic!("the first check must acquire its authority: {outcome:?}");
    };
    let fetched = fetch_authoritative_release(
        Some(candidate),
        &mut |url, _| channel.serve(url),
        &channel.policy(&masters),
    );
    let (manifest, _, _) = fetched.selected.expect("the real signature chain accepts");
    let commit = manifest.commit.as_deref().expect("signed identity");
    write_ready(&staging, manifest.build_number, commit, &manifest.sha256);
    write_bundle_identity(&staging, manifest.build_number, commit);
    // The ledger the check writes after this stage, written directly rather than
    // through the process-global carry (`set_latest_tag` + `record`), which a
    // concurrently running apply-lane test may clear between the two calls.
    std::fs::write(
        &staging.status,
        format!(
            "schema = 1\noutcome = \"verified stage published\"\ncurrent_build = \
             {WEB_BUILD}\nlatest_tag = {WEB_TAG:?}\nlatest_source = \"alabsystems/aterm\"\n\
             latest_authorized_build = {WEB_BUILD}\nlatest_release_build = {}\n",
            manifest.build_number
        ),
    )
    .unwrap();
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads, vec![evergreen_url()]);
    assert!(matches!(outcome, Ok(Acquisition::UpToDate { .. })));

    // Publication invalidates a previous marker before committing the next
    // one. A failed commit or interrupted writer leaves exactly this state,
    // with no call to retire_published to clear the cached tag.
    std::fs::remove_file(&staging.ready).unwrap();
    assert!(Ready::read_publishable(&staging).is_none());
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads, vec![evergreen_url()]);
    let Ok(Acquisition::Proceed(candidate)) = outcome else {
        panic!("the unchanged head must retry the lost stage: {outcome:?}");
    };
    let mut gets = Vec::new();
    let recovered = fetch_authoritative_release(
        Some(candidate),
        &mut |url, _| {
            gets.push(url.to_string());
            channel.serve(url)
        },
        &channel.policy(&masters),
    );
    assert_eq!(
        recovered.selected.unwrap().0.build_number,
        manifest.build_number
    );
    assert_eq!(
        gets.len(),
        4,
        "recovery repeats the complete signature chain"
    );
    assert!(gets.iter().all(|url| url.starts_with(&tag_prefix(WEB_TAG))));

    // Historical negative control: trusting the bare tag still takes the
    // shortcut and would never reach those four verification fetches.
    assert!(matches!(
        resolve_web_head(&staging, WEB_BUILD, &source, Some(WEB_TAG), &mut |_| {
            redirect_to(WEB_TAG)
        },),
        Ok(WebHead::Unchanged { .. })
    ));
    crate::status::clear_check_note();
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// Every way the pointer can fail ends the check WITHOUT an API request: a 404 is
/// the loud standing state (announced, stranded), a 429/5xx is a deferral (no
/// health entry), a refused redirect or a transport failure is the historical
/// `network`-class failure — and a refusal is never interpreted, so nothing is
/// fetched from wherever it pointed.
#[test]
fn every_web_lane_failure_path_ends_without_an_api_request() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("failures");
    crate::status::clear_check_note();

    // 404: stranded, and the ledger says why.
    crate::unreadable::clear();
    let (outcome, heads) = acquire_web(&staging, None, || {
        Ok(HeadAnswer {
            code: 404,
            location: None,
        })
    });
    assert_eq!(heads.len(), 1);
    assert!(matches!(outcome, Ok(Acquisition::Ended)), "{outcome:?}");
    assert!(crate::unreadable::is_stranded());
    let text = std::fs::read_to_string(&staging.status).unwrap();
    assert!(text.contains("cannot read its release channel"), "{text}");
    assert!(text.contains("no published release"), "{text}");
    assert!(
        !staging.health().exists(),
        "a standing state is not a fault"
    );
    crate::unreadable::clear();

    // 429 / 5xx: deferred, no health entry, the latch set for the loop.
    for code in [429u16, 503] {
        let (outcome, heads) = acquire_web(&staging, None, || {
            Ok(HeadAnswer {
                code,
                location: None,
            })
        });
        assert_eq!(heads.len(), 1);
        assert!(
            matches!(outcome, Ok(Acquisition::Ended)),
            "{code}: {outcome:?}"
        );
        assert!(rate_limited(), "{code}: the loop lengthens its wait");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(text.contains("deferred"), "{code}: {text}");
        assert!(!staging.health().exists(), "{code}: weather is not a fault");
    }

    // A refused redirect (another repository, the same asset name) and a transport
    // failure: `network`-class, and NOTHING is fetched from the refused location.
    let elsewhere = "https://github.com/someone/aterm/releases/download/v0.10.0/aterm-appcast.toml";
    for (label, answer) in [
        (
            "refused",
            Ok(HeadAnswer {
                code: 302,
                location: Some(elsewhere.to_string()),
            }),
        ),
        (
            "transport",
            Err(HttpError::Transport("curl HEAD x failed".into())),
        ),
        (
            "unexpected",
            Ok(HeadAnswer {
                code: 200,
                location: None,
            }),
        ),
    ] {
        let before = crate::health::Health::read(&staging.health()).network_failures;
        let (outcome, heads) = acquire_web(&staging, None, || answer.clone());
        assert_eq!(heads.len(), 1, "{label}");
        let Err(error) = outcome else {
            panic!("{label}: expected a failure, got {outcome:?}");
        };
        if label == "refused" {
            assert!(error.contains("refuses to follow"), "{error}");
            assert!(
                !error.contains("someone/aterm"),
                "a refusal never echoes the server's string: {error}"
            );
        }
        assert_eq!(
            crate::health::Health::read(&staging.health()).network_failures,
            before + 1,
            "{label}: booked as network"
        );
    }
    note_readable(&test_source());
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// Invariant (d): the pointer alone can never choose what is installed. The tag it
/// names must equal `v` + the signed manifest's `version` AND the tag inside the
/// signed manifest's `url`; a manifest that fails either bind is REJECTED, and the
/// manifest's URL is never the one downloaded.
#[test]
fn the_pointers_tag_the_manifest_version_and_the_manifest_url_must_agree() {
    let source = test_source();
    let derived = tag_url(WEB_TAG, "aterm-0.10.0.dmg");
    let manifest = |url: Option<&str>| {
        let mut text = String::from_utf8(manifest_bytes("0.10.0", 10, 0)).unwrap();
        if let Some(url) = url {
            text.push_str(&format!("url = {url:?}\n"));
        }
        Manifest::parse(&text).unwrap()
    };
    web_container_url_agrees(&source, WEB_TAG, &manifest(Some(&derived))).unwrap();
    for wrong in [
        // The same appcast copied onto another tag.
        Some(tag_url("v0.9.0", "aterm-0.10.0.dmg")),
        // Another repository's copy of the container.
        Some("https://github.com/someone/aterm/releases/download/v0.10.0/aterm-0.10.0.dmg".into()),
        // The evergreen alias, which would let a moving pointer choose.
        Some("https://github.com/alabsystems/aterm/releases/latest/download/aterm.dmg".into()),
        // No URL at all: the check requires the bind.
        None,
    ] {
        let error = web_container_url_agrees(&source, WEB_TAG, &manifest(wrong.as_deref()))
            .expect_err("must refuse");
        assert!(
            error.contains("refusing") || error.contains("requires"),
            "{wrong:?}: {error}"
        );
    }

    // The version half, on the real fetch path: a pointer at v0.11.0 whose appcast
    // says 0.10.0 is refused as a manifest defect, never staged.
    let channel = web_channel(Some(&derived));
    let masters = [channel.master_pub.as_str()];
    let candidate = web_release(&source, "v0.11.0").unwrap();
    let mut gets = Vec::new();
    let mut download = |url: &str, _max: u64| {
        gets.push(url.to_string());
        // Serve WEB_TAG's bytes under v0.11.0's derived names.
        let name = url.rsplit('/').next().unwrap().to_string();
        channel.serve(&tag_url(WEB_TAG, &name))
    };
    let fetched =
        fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
    assert!(fetched.selected.is_none());
    assert!(
        fetched.manifest_rejected,
        "version 0.10.0 does not bind to v0.11.0"
    );
    assert!(
        gets.iter().all(|u| u.starts_with(&tag_prefix("v0.11.0"))),
        "every GET was addressed by the pointer's tag: {gets:?}"
    );
}

/// A pointer that moves DURING a check cannot mix two releases: the HEAD is asked
/// exactly once, and every GET is addressed by the tag that one answer named.
#[test]
fn a_pointer_that_moves_mid_check_cannot_mix_two_releases() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("moving");
    let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
    let masters = [channel.master_pub.as_str()];
    let mut calls = 0;
    let (outcome, heads) = acquire_web(&staging, None, || {
        calls += 1;
        redirect_to(if calls == 1 { WEB_TAG } else { "v0.11.0" })
    });
    assert_eq!(heads.len(), 1, "the pointer is consulted once");
    let Ok(Acquisition::Proceed(candidate)) = outcome else {
        panic!("{outcome:?}");
    };
    let mut gets = Vec::new();
    let mut download = |url: &str, _max: u64| {
        gets.push(url.to_string());
        channel.serve(url)
    };
    let fetched =
        fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
    assert!(fetched.selected.is_some());
    assert!(
        gets.iter().all(|u| u.starts_with(&tag_prefix(WEB_TAG))),
        "{gets:?}"
    );
    assert!(gets.iter().all(|u| !u.contains("v0.11.0")), "{gets:?}");
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// Invariant (c) on the download host: a roster that cannot be FETCHED refuses the
/// release as a transport failure — there is nothing else a client could accept it on.
#[test]
fn a_roster_that_cannot_be_fetched_on_the_web_lane_refuses_as_transport() {
    let source = test_source();
    let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
    let masters = [channel.master_pub.as_str()];
    let candidate = web_release(&source, WEB_TAG).unwrap();
    let mut gets = Vec::new();
    let mut download = |url: &str, _max: u64| {
        gets.push(url.to_string());
        if url.ends_with(aterm_update_core::roster::ROSTER_ASSET) {
            return Err("HTTP 404: the release does not carry this asset".to_string());
        }
        channel.serve(url)
    };
    let fetched =
        fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
    assert!(fetched.selected.is_none(), "refused");
    assert!(
        fetched.appcast_fetch_error,
        "transport, not a manifest verdict"
    );
    assert!(!fetched.manifest_rejected);
    assert!(!fetched.asset_fetch_rate_limited);
    assert!(fetched.attribution.is_none());
    assert!(
        gets.iter().all(|u| !aterm_update_core::cdn::is_api_host(u)),
        "{gets:?}"
    );
    // …and a web-host 429 on the same leg is the deferrable weather, still refused.
    let mut download = |url: &str, _max: u64| {
        if url.ends_with(aterm_update_core::roster::ROSTER_ASSET) {
            return Err("rate limited (HTTP 429) fetching an asset".to_string());
        }
        channel.serve(url)
    };
    let candidate = web_release(&source, WEB_TAG).unwrap();
    let fetched =
        fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
    assert!(fetched.selected.is_none() && fetched.appcast_fetch_error);
    assert!(fetched.asset_fetch_rate_limited);
}

/// THE STEADY-STATE SHORTCUT IS BOUND TO THIS MACHINE'S DECISION. A ledger naming
/// the pointer's tag is "up to date" only when it was written by THIS build against
/// THIS source; recorded by another build (a manual downgrade, an applied stage) or
/// against another repository (a repointed updater), the release is re-judged —
/// one HEAD and the tag-specific GETs, still zero API. And a RETIRED stage clears
/// the tag, so the next check re-fetches and re-stages rather than answering "up to
/// date" on a build that is gone.
#[test]
fn the_shortcut_is_bound_to_the_recording_build_and_source_and_cleared_by_retirement() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("shortcut-binding");
    let source = test_source();
    // The same tag, recorded by another build: re-judged.
    for other in [WEB_BUILD - 1, WEB_BUILD + 1] {
        write_ledger(&staging, WEB_TAG, other, "alabsystems/aterm");
        let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert_eq!(heads, vec![evergreen_url()]);
        assert!(
            matches!(outcome, Ok(Acquisition::Proceed(_))),
            "build {other}: {outcome:?}"
        );
    }
    // The same tag, recorded against another repository: re-judged.
    write_ledger(&staging, WEB_TAG, WEB_BUILD, "example/mirror");
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads.len(), 1);
    assert!(
        matches!(outcome, Ok(Acquisition::Proceed(_))),
        "{outcome:?}"
    );
    // A ledger that recorded no source (an earlier build of this design): re-judged.
    std::fs::write(
        &staging.status,
        format!("schema = 1\ncurrent_build = {WEB_BUILD}\nlatest_tag = {WEB_TAG:?}\n"),
    )
    .unwrap();
    let (outcome, _) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert!(
        matches!(outcome, Ok(Acquisition::Proceed(_))),
        "{outcome:?}"
    );
    // This build, this source: the steady state — written the way the check writes
    // it, through `set_latest_tag` + `record`.
    crate::status::clear_check_note();
    crate::status::set_latest_tag(WEB_TAG, &source, WEB_BUILD, WEB_BUILD);
    crate::status::record(&staging, WEB_BUILD, "staged 0.10.0 (build 10)");
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads.len(), 1);
    assert!(
        matches!(outcome, Ok(Acquisition::UpToDate { .. })),
        "{outcome:?}"
    );
    // RETIREMENT ("so the next check re-stages"): the unchanged pointer now
    // proceeds to a full re-fetch — and every request is still off the API.
    staging.retire_published();
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads, vec![evergreen_url()]);
    let Ok(Acquisition::Proceed(candidate)) = outcome else {
        panic!("a retired stage forces a re-fetch: {outcome:?}");
    };
    assert_eq!(candidate.release.tag_name, WEB_TAG);
    for asset in &candidate.release.assets {
        assert!(!aterm_update_core::cdn::is_api_host(&asset.url));
    }
    crate::status::clear_check_note();
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE URL BIND ON A REPOINTED SOURCE. The publisher signs `url` under the
/// compiled-in channel's slug, so an updater repointed at a mirror
/// carrying the upstream-signed appcast must accept it — the bind is on the TAG and
/// the DMG, which invariant (d) needs, not on the repository, which
/// `parse_location` already scopes. Any other slug, tag or container is refused, and
/// so is a manifest with no `url` at all.
#[test]
fn the_url_bind_accepts_the_source_or_the_compiled_in_channel_and_nothing_else() {
    let mirror = Source {
        owner: "example".into(),
        repo: "mirror".into(),
    };
    let with_url = |url: Option<&str>| Manifest {
        url: url.map(str::to_string),
        ..candidate_manifest()
    };
    let dmg = candidate_manifest().dmg;
    let tag = "v0.6.0";
    let upstream = tag_url(tag, &dmg);
    web_container_url_agrees(&mirror, tag, &with_url(Some(&upstream)))
        .expect("the upstream-signed appcast is accepted on a mirror");
    web_container_url_agrees(&test_source(), tag, &with_url(Some(&upstream)))
        .expect("…and on the compiled-in channel");
    let own = aterm_update_core::cdn::release_download_url("example", "mirror", tag, &dmg).unwrap();
    web_container_url_agrees(&mirror, tag, &with_url(Some(&own)))
        .expect("a mirror's own re-signed appcast is accepted too");
    let cased =
        aterm_update_core::cdn::release_download_url("Example", "MIRROR", tag, &dmg).unwrap();
    web_container_url_agrees(&mirror, tag, &with_url(Some(&cased)))
        .expect("slugs compare case-insensitively, as GitHub does");
    for (why, bad) in [
        (
            "another repository",
            aterm_update_core::cdn::release_download_url("someone", "else", tag, &dmg).unwrap(),
        ),
        ("another tag", tag_url("v0.7.0", &dmg)),
        ("another container", tag_url(tag, "aterm-0.7.0.dmg")),
        (
            "another host",
            format!("https://evil.example/alabsystems/aterm/releases/download/{tag}/{dmg}"),
        ),
        (
            "the evergreen URL",
            aterm_update_core::pointer::latest_download_url("alabsystems", "aterm", &dmg).unwrap(),
        ),
        ("a query", format!("{upstream}?x=1")),
        ("an extra segment", format!("{upstream}/x")),
    ] {
        let error = web_container_url_agrees(&mirror, tag, &with_url(Some(&bad))).expect_err(why);
        assert!(error.contains("refusing"), "{why}: {error}");
    }
    assert!(
        web_container_url_agrees(&mirror, tag, &with_url(None))
            .unwrap_err()
            .contains("no container `url`")
    );
}

/// A FAILED CHECK IS NOT A COMPLETED ONE. The receipt is what the
/// machine-wide checker gate reads to skip an interval, and what `checked_at`
/// reports as the last time this channel was asked. A check that ended in a
/// transport error asked nobody, and stamping it would make the failing process
/// skip on its own stamp.
#[test]
fn only_a_check_that_reached_the_channel_leaves_a_receipt() {
    assert!(
        check_leaves_a_receipt(&Ok(None), false),
        "up to date is a completed check and must dedup the siblings"
    );
    assert!(
        check_leaves_a_receipt(&Ok(Some("0.86.0".into())), false),
        "a staged build is a completed check"
    );
    assert!(
        !check_leaves_a_receipt(
            &Err("dns error: nodename nor servname provided".into()),
            false
        ),
        "a resolver failure reached no channel: it must not stamp the receipt \
         every checker on this machine reads as this interval's completed check"
    );
    assert!(
        check_leaves_a_receipt(&Err("the release host closed the connection".into()), true),
        "a recorded deferral's machine-wide retreat is kept even when a later leg \
         failed"
    );
}

/// THE PUBLISHED v0.90.0 RELEASE VERIFIES UNDER THE COMMITTED PAPER MASTER ALONE.
///
/// These are the exact bytes the channel serves for v0.90.0 (appcast, its signature,
/// the machine roster and its master signature — `tests/fixtures/v0.90.0`), signed by
/// roster machine m3, whose key was never in any compiled-in keyset. The client below
/// is THIS tree's: `pins::PAPER_MASTER_PUBKEYS` and nothing else. So a current-format
/// release verifies with K1 retired, which is what every client from v0.21.0 on has
/// done all along. Negative controls: one flipped appcast byte, or a master this
/// build does not pin, refuses.
#[test]
fn a_published_release_verifies_under_the_committed_master_alone() {
    const APPCAST: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-appcast.toml");
    const APPCAST_SIG: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-appcast.toml.sig");
    const ROSTER: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-machines.toml");
    const ROSTER_SIG: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-machines.toml.sig");
    // 2026-09-21T20:00:00Z, just after the release; the roster is valid far past it.
    const PUBLISHED_AT: i64 = 1_790_020_800;
    let tag = "v0.90.0";
    let serve = |appcast: &'static [u8]| {
        move |url: &str, _max: u64| -> Result<Vec<u8>, String> {
            let name = url.rsplit('/').next().unwrap_or_default();
            match name {
                APPCAST_ASSET => Ok(appcast.to_vec()),
                APPCAST_SIG_ASSET => Ok(APPCAST_SIG.to_vec()),
                "aterm-machines.toml" => Ok(ROSTER.to_vec()),
                "aterm-machines.toml.sig" => Ok(ROSTER_SIG.to_vec()),
                other => Err(format!("HTTP 404: no such asset {other}")),
            }
        }
    };
    let policy = |masters: &'static [&'static str]| RosterPolicy {
        master_pubkeys: masters,
        floor_seq: 0,
        now_unix: PUBLISHED_AT,
        floor_refresh: None,
    };
    let fetched = fetch_authoritative_release(
        Some(web_release(&test_source(), tag).unwrap()),
        &mut serve(APPCAST),
        &policy(aterm_update_core::pins::PAPER_MASTER_PUBKEYS),
    );
    let (manifest, release, _) = fetched.selected.as_ref().unwrap_or_else(|| {
        panic!(
            "the published release must verify: {:?}",
            fetched.rejection_reason
        )
    });
    assert_eq!(manifest.version, "0.90.0");
    assert_eq!(manifest.build_number, 1_790_019_739);
    web_container_url_agrees(&test_source(), &release.tag_name, manifest)
        .expect("the signed url binds to the tag");
    let who = fetched.attribution.as_ref().expect("attributed");
    assert_eq!(who.machine_id, "m3");
    assert_eq!(fetched.observed_roster_seq, Some(4));

    // Negative control 1: one flipped byte of the appcast refuses.
    let mut tampered = APPCAST.to_vec();
    let last = tampered.len() - 2;
    tampered[last] ^= 0x01;
    let tampered: &'static [u8] = Vec::leak(tampered);
    let refused = fetch_authoritative_release(
        Some(web_release(&test_source(), tag).unwrap()),
        &mut serve(tampered),
        &policy(aterm_update_core::pins::PAPER_MASTER_PUBKEYS),
    );
    assert!(refused.selected.is_none() && refused.manifest_rejected);

    // Negative control 2: a master this build does not pin refuses the roster.
    let stranger = pub_b64(&OTHER_MASTER_FIXTURE);
    let stranger: &'static [&'static str] = Vec::leak(vec![&*String::leak(stranger)]);
    let refused = fetch_authoritative_release(
        Some(web_release(&test_source(), tag).unwrap()),
        &mut serve(APPCAST),
        &policy(stranger),
    );
    assert!(refused.selected.is_none() && refused.manifest_rejected);
}

/// A HEAD THAT IS NOT AN APP RELEASE is said, once per check, as a healthy outcome
/// — and nothing looks past it. Both shapes: a tag this client does not install from
/// (`OtherTag`, decided by the pointer alone), and a canonical tag whose appcast
/// answers 404 (a source-only release). One HEAD, no API, no failure streak, no
/// strand, and the tag is NOT recorded as authorized so the next check reads it
/// again. Negative control: the same head WITH its appcast proceeds.
#[test]
fn a_head_that_is_not_an_app_release_ends_the_check_healthy_and_unauthorized() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("non-app-head");
    crate::status::clear_check_note();
    crate::unreadable::clear();
    let index_tag = "atpkg-index-44";
    let (outcome, heads) = acquire_web(&staging, None, || {
        Ok(HeadAnswer {
            code: 302,
            location: Some(tag_url(index_tag, APPCAST_ASSET)),
        })
    });
    assert_eq!(heads, vec![evergreen_url()], "one HEAD and nothing else");
    assert!(matches!(outcome, Ok(Acquisition::Ended)), "{outcome:?}");
    let text = std::fs::read_to_string(&staging.status).unwrap();
    assert!(
        text.contains("channel head atpkg-index-44 has no app manifest yet"),
        "{text}"
    );
    assert!(!text.contains("latest_tag"), "never authorized: {text}");
    let h = crate::health::Health::read(&staging.health());
    assert_eq!(h.acquisition_failures(), 0, "not a failure");
    assert!(!crate::unreadable::is_stranded(), "not a strand");

    // The source-only shape: the tag is an app tag, its appcast 404s.
    let fetched = fetch_authoritative_release(
        Some(web_release(&test_source(), WEB_TAG).unwrap()),
        &mut |url: &str, _max: u64| Err(format!("HTTP 404: no such asset {url}")),
        &RosterPolicy::INERT,
    );
    assert!(fetched.selected.is_none() && fetched.appcast_missing);
    record_head_without_app(&staging, WEB_BUILD, WEB_TAG);
    let text = std::fs::read_to_string(&staging.status).unwrap();
    assert!(
        text.contains("channel head v0.10.0 has no app manifest yet"),
        "{text}"
    );
    assert!(
        crate::status::latest_tag(&staging, WEB_BUILD, &test_source()).is_none(),
        "a head with no app manifest is never the authorized tag"
    );
    // Negative control: an app tag proceeds to acquisition.
    let (outcome, _) = acquire_web(&staging, None, || redirect_to(WEB_TAG));
    assert!(
        matches!(outcome, Ok(Acquisition::Proceed(_))),
        "{outcome:?}"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// NOTHING LOOKS PAST THE HEAD, however many tags on the host would answer. The
/// download host serves a
/// PRERELEASE's assets with the same 302 into the asset store as a release's —
/// measured on `atpkg-claude-2026090801`, a prerelease — and the publisher's
/// promotion gate is `latest` alone, so a check that probed the tags past a head it
/// cannot install from would elect an in-flight or retired cut that was never
/// promoted (and judge it under its own roster, not the head's). A machine that
/// authorized v0.9.0, with `latest` on a tag it does not install from and EVERY
/// derived appcast on the host answering 302, spends the pointer's one HEAD and
/// ends on the head: healthy, the head unauthorized, the ledger's tag kept.
#[test]
fn nothing_looks_past_a_head_even_when_every_tag_past_it_would_answer() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("no-look-past");
    let source = test_source();
    crate::status::clear_check_note();
    crate::unreadable::clear();
    set_head_in_flight(None);
    write_ledger(&staging, "v0.9.0", WEB_BUILD, "alabsystems/aterm");
    let mut heads = Vec::new();
    let mut head = |url: &str| -> Result<HeadAnswer, HttpError> {
        heads.push(url.to_string());
        Ok(HeadAnswer {
            code: 302,
            location: Some(if url == evergreen_url() {
                tag_url("atpkg-index-44", APPCAST_ASSET)
            } else {
                // Any other tag's derived appcast: served, as a prerelease's is.
                "https://release-assets.githubusercontent.com/github-production-release-asset/x"
                    .to_string()
            }),
        })
    };
    let outcome = acquire(&staging, WEB_BUILD, &source, &mut head);
    assert!(matches!(outcome, Ok(Acquisition::Ended)), "{outcome:?}");
    assert_eq!(
        heads,
        [evergreen_url()],
        "the pointer's HEAD and nothing past it"
    );
    let text = std::fs::read_to_string(&staging.status).unwrap();
    assert!(
        text.contains(
            "channel head atpkg-index-44 has no app manifest yet — nothing to install from it"
        ),
        "{text}"
    );
    assert_eq!(
        crate::status::latest_tag(&staging, WEB_BUILD, &source).as_deref(),
        Some("v0.9.0"),
        "the head is never authorized, and the ledger keeps what it had"
    );
    assert_eq!(
        crate::health::Health::read(&staging.health()).acquisition_failures(),
        0
    );
    assert_eq!(head_in_flight(), None, "not an app build in flight");
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE HEAD IN FLIGHT (a release published source-first meets a check before its
/// appcast lands). A source-only head that OUTRANKS the last
/// tag authorized for its source — or any tag, when none is — is an app
/// build in flight, and the check latches it for the receipt to count. Negative
/// controls: a head at or below the authorized tag (GitHub's `latest` can name a
/// re-published older release), a tag this client does not install from, and a
/// retired two-component tag are not; the other-tag pointer latches nothing.
#[test]
fn a_source_only_head_above_the_authorized_tag_is_an_app_build_in_flight() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(app_build_in_flight("v0.92.0", Some("v0.91.0")));
    assert!(app_build_in_flight("v0.92.0", None));
    assert!(!app_build_in_flight("v0.91.0", Some("v0.91.0")));
    assert!(!app_build_in_flight("v0.90.0", Some("v0.91.0")));
    assert!(!app_build_in_flight("atpkg-index-46", None));
    assert!(!app_build_in_flight("v0.61", None));

    let staging = Staging::scratch("head-in-flight");
    let source = test_source();
    crate::status::clear_check_note();
    for (authorized, latched) in [
        ("v0.9.0", Some((WEB_TAG.to_string(), 0))),
        ("v0.11.0", None),
    ] {
        write_ledger(&staging, authorized, WEB_BUILD, "alabsystems/aterm");
        set_head_in_flight(None);
        record_source_only_head(&staging, WEB_BUILD, &source, WEB_TAG);
        assert_eq!(head_in_flight(), latched, "authorized {authorized}");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(
            text.contains("channel head v0.10.0 has no app manifest yet"),
            "{text}"
        );
    }
    let _ = std::fs::remove_file(&staging.status);
    set_head_in_flight(None);
    record_source_only_head(&staging, WEB_BUILD, &source, WEB_TAG);
    assert_eq!(
        head_in_flight(),
        Some((WEB_TAG.to_string(), 0)),
        "none authorized"
    );

    set_head_in_flight(None);
    let (outcome, _) = acquire_web(&staging, None, || {
        Ok(HeadAnswer {
            code: 302,
            location: Some(tag_url("atpkg-index-44", APPCAST_ASSET)),
        })
    });
    assert!(matches!(outcome, Ok(Acquisition::Ended)), "{outcome:?}");
    assert_eq!(
        head_in_flight(),
        None,
        "a tag this client does not install from"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// EVERY BUILD ON THE MACHINE CALLS THE SAME HEAD IN FLIGHT. The count the quick
/// ladder keys on is machine-wide, so the predicate it counts must be too: with
/// the ledger at v0.93.0 (build 42) and GitHub's `latest` naming a re-published
/// source-only v0.92.0, builds 41 and 42 both call it below the authorized tag,
/// not in flight — and a head ABOVE it is in flight for both (the positive
/// control). Another source's authorized tag decides nothing.
#[test]
fn every_build_calls_the_same_head_in_flight() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("head-in-flight-builds");
    let source = test_source();
    crate::status::clear_check_note();
    let latched = |build: u64, head: &str| {
        set_head_in_flight(None);
        record_source_only_head(&staging, build, &source, head);
        head_in_flight().is_some()
    };
    // The ledger: build 42 authorized v0.93.0.
    write_ledger(&staging, "v0.93.0", 42, "alabsystems/aterm");
    assert!(!latched(42, "v0.92.0"), "the recording build");
    assert!(!latched(41, "v0.92.0"), "another build, the same answer");
    assert!(latched(42, "v0.94.0"));
    assert!(latched(41, "v0.94.0"));
    // Through the receipt: the two builds' checks no longer alternate a
    // count of 0 and 1 on a head below the authorized tag.
    let counts: Vec<u32> = [42, 41, 42, 41]
        .into_iter()
        .map(|build| {
            set_head_in_flight(None);
            record_source_only_head(&staging, build, &source, "v0.92.0");
            let found = head_in_flight().map(|(tag, _)| tag);
            stamp_receipt(&staging, build, &source, &Ok(None), false, found).map_or(0, |(_, n)| n)
        })
        .collect();
    assert_eq!(counts, [0, 0, 0, 0]);
    // Another repository's authorized tag is no answer about this one.
    write_ledger(&staging, "v0.93.0", 42, "someone/else");
    assert!(latched(41, "v0.92.0"));
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// The count the quick ladder keys on is the RECEIPT's, machine-wide: consecutive
/// completed checks that found the same head in flight count up; a deferral, a
/// check that found none and a different head each end the run. A FAILED check
/// writes no receipt, so it neither counts nor ends the run: the next completed
/// check carries the count on (1, fail, 2 — erring toward fewer quick checks).
#[test]
fn a_head_in_flight_is_counted_on_the_receipt_of_completed_checks_only() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("head-in-flight-receipt");
    let source = test_source();
    let healthy: Result<Option<String>, String> = Ok(None);
    let stamp = |result: &Result<Option<String>, String>, deferred: bool, head: Option<&str>| {
        stamp_receipt(
            &staging,
            WEB_BUILD,
            &source,
            result,
            deferred,
            head.map(str::to_owned),
        )
    };
    let v92 = Some("v0.92.0");
    assert_eq!(stamp(&healthy, false, v92), Some(("v0.92.0".into(), 1)));
    assert_eq!(stamp(&healthy, false, v92), Some(("v0.92.0".into(), 2)));
    assert_eq!(
        stamp(&Ok(Some("0.91.0".into())), false, v92),
        Some(("v0.92.0".into(), 3)),
        "a check that staged something still counts the head it found"
    );
    // A deferral records its own window and no head; the next run starts over.
    assert_eq!(stamp(&healthy, true, v92), None);
    assert_eq!(stamp(&healthy, false, v92), Some(("v0.92.0".into(), 1)));
    // A failed check leaves the receipt alone, and answers no head.
    let before = std::fs::read(crate::check_receipt::path(&staging)).unwrap();
    assert_eq!(stamp(&Err("dns".into()), false, v92), None);
    assert_eq!(
        std::fs::read(crate::check_receipt::path(&staging)).unwrap(),
        before
    );
    assert_eq!(stamp(&healthy, false, v92), Some(("v0.92.0".into(), 2)));
    // A different head is a different release in flight.
    assert_eq!(
        stamp(&healthy, false, Some("v0.93.0")),
        Some(("v0.93.0".into(), 1))
    );
    // A check that found none ends the run.
    assert_eq!(stamp(&healthy, false, None), None);
    assert_eq!(
        stamp(&healthy, false, Some("v0.93.0")),
        Some(("v0.93.0".into(), 1))
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

// -----------------------------------------------------------------------------
// The check channel's ledger laws; see also `crate::check_channel_audit_tests`.
// -----------------------------------------------------------------------------

/// THE COVERED-STAGE LINE KEEPS THE STANDING APPLY VERDICT. Between apply
/// attempts, a check that finds the stage already covering the candidate rewrites
/// `status.toml`'s one `outcome` line; when `Health::last_apply_failure_target_build`
/// names this very artifact, that line carries the apply lane's refusal rather
/// than a bare "verified and ready to apply".
#[test]
fn check_channel_audit_a_covered_stage_line_keeps_the_standing_apply_verdict() {
    let staging = Staging::scratch("covered-vs-apply");
    let manifest = candidate_manifest();
    let commit = manifest.commit.as_deref().unwrap();
    write_ready(&staging, manifest.build_number, commit, &manifest.sha256);
    write_bundle_identity(&staging, manifest.build_number, commit);
    assert!(
        publishable_stage_covers(&staging, &manifest),
        "precondition"
    );
    let running = 1;
    // The apply lane's terminal verdict on THIS artifact, exactly as
    // `crate::record_apply_failure` leaves both ledgers.
    let reason = "PreparationFailed: the installed bundle at /Applications/aterm.app \
                  cannot be the rollback source the swap installs";
    crate::health::Health::record_apply_failure(
        &staging.health(),
        running,
        manifest.build_number,
        reason,
    );
    crate::status::record(
        &staging,
        running,
        &format!("staged build did not apply: {reason}"),
    );
    // The next check, an interval later, finds the stage covering the candidate.
    record_covered_stage_status(&staging, running, &manifest);
    let text = std::fs::read_to_string(&staging.status).expect("status written");
    let outcome = text
        .parse::<aterm_toml::Value>()
        .ok()
        .and_then(|v| {
            v.get("outcome")
                .and_then(aterm_toml::Value::as_str)
                .map(str::to_string)
        })
        .expect("an outcome line");
    assert!(
        outcome.contains("did not apply") || outcome.contains("rollback source"),
        "the covered-stage line must carry the standing apply verdict on the very \
         build it calls ready, not erase it: {outcome}"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}
