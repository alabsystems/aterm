// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn b64(raw: &[u8]) -> String {
    aterm_codec::base64::encode(raw).expect("test key material is far below MAX_INPUT_LEN")
}

fn test_source() -> Source {
    Source {
        owner: "alabsystems".into(),
        repo: "aterm".into(),
    }
}

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

fn manifest_bytes(version: &str, build_number: u64, min_build: u64) -> Vec<u8> {
    format!(
        "schema = 1\nversion = \"{version}\"\nbuild_number = {build_number}\n\
         sha256 = \"{}\"\ndmg = \"aterm-{version}.dmg\"\nmin_build = {min_build}\n",
        "ab".repeat(32),
    )
    .into_bytes()
}

#[test]
fn stage_selection_prefers_the_zip_and_falls_back_to_the_dmg() {
    const VERSION: &str = "0.54.0";
    let zip_name = format!("aterm-{VERSION}-mac.zip");
    let zip_digest = "cd".repeat(32);
    let release = web_release(&test_source(), "v0.54.0").unwrap().release;

    let dmg_only = candidate_manifest();
    let chosen = select_stage_artifact(&release, &dmg_only, VERSION).unwrap();
    assert_eq!(chosen.container, Container::Dmg);
    assert_eq!(chosen.name, "aterm-0.54.0.dmg");
    assert_eq!(chosen.sha256, dmg_only.sha256);
    assert_eq!(release.assets[chosen.asset_index].name, chosen.name);

    let mut zipped = candidate_manifest();
    zipped.zip = Some(zip_name.clone());
    zipped.zip_sha256 = Some(zip_digest.clone());
    let chosen = select_stage_artifact(&release, &zipped, VERSION).unwrap();
    assert_eq!(chosen.container, Container::Zip);
    assert_eq!(chosen.name, zip_name);
    assert_eq!(chosen.sha256, zip_digest);
    assert_eq!(release.assets[chosen.asset_index].name, chosen.name);

    let mut no_digest = zipped.clone();
    no_digest.zip_sha256 = None;
    let mut noncanonical = zipped.clone();
    noncanonical.zip = Some("aterm-mac.zip".into());
    for (label, manifest) in [
        ("no digest to check the bytes against", &no_digest),
        ("noncanonical zip name", &noncanonical),
    ] {
        let chosen = select_stage_artifact(&release, manifest, VERSION)
            .unwrap_or_else(|error| panic!("{label} must still update: {error}"));
        assert_eq!(chosen.container, Container::Dmg, "{label}");
        assert_eq!(chosen.sha256, manifest.sha256, "{label}");
    }

    let mut bad_dmg = zipped;
    bad_dmg.dmg = "aterm.dmg".into();
    select_stage_artifact(&release, &bad_dmg, VERSION)
        .expect_err("a noncanonical DMG must refuse the whole release");
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
    let answer = rx.recv_timeout(std::time::Duration::from_secs(60));
    let _ = std::os::unix::fs::OpenOptionsExt::custom_flags(
        std::fs::OpenOptions::new().write(true),
        libc::O_NONBLOCK,
    )
    .open(&fifo);
    let _ = hasher.join();
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
    assert!(crate::is_passing_refusal(&error), "{error}");
}

#[test]
fn an_asset_fetch_that_fails_just_after_a_wake_books_network() {
    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    let reset = "curl download failed (exit status: 56): curl: (56) Recv failure: Connection \
                 reset by peer";
    for after_wake in [true, false] {
        let staging = Staging::scratch("asset-fetch-after-wake");
        crate::health::Health::record_failure(&staging.health(), "stage", "standing verdict");
        {
            let _woke = crate::AfterWakeScope::enter(after_wake);
            book_download_failure(&staging, running, &manifest, "zip", reset, None)
                .expect_err("the check fails");
        }
        let h = crate::health::Health::read(&staging.health());
        if after_wake {
            assert_eq!((h.network_failures, h.pipeline_failures), (1, 0), "{h:?}");
            assert_eq!(h.stage_failures, 1, "weather keeps the diagnosis: {h:?}");
        } else {
            assert_eq!((h.network_failures, h.pipeline_failures), (0, 1), "{h:?}");
            assert_eq!(h.stage_failures, 0, "one check, one diagnosis: {h:?}");
        }
        let _ = std::fs::remove_dir_all(&staging.root);
    }
    assert!(
        !crate::check_follows_wake(),
        "the scope restores the thread's answer"
    );
}

#[test]
fn a_check_a_person_asks_for_passes_a_full_disks_memo() {
    let staging = Staging::scratch("full-disk-person");
    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
    let written = "curl download failed (exit status: 23): curl: (23) Failure writing \
                   output to destination";
    for _ in 0..3 {
        book_download_failure(&staging, running, &manifest, "zip", written, Some(1 << 20))
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

    let verdict = "codesign --verify (team-pinned requirement) failed: code object is not \
                   signed at all";
    let _ = book_stage_failure(&staging, running, &manifest, verdict, now);
    assert!(
        stage_backoff(&staging, &manifest, running, now, Asked::Person).is_some(),
        "a verdict is a finding about the build; a person asking changes nothing"
    );
    crate::manifest::FailedMark::clear_backoff(&staging.failed());
    crate::manifest::quarantine_artifact(
        &staging,
        manifest.build_number,
        &manifest.sha256,
        aterm_durable::Flush::Durable,
    );
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
    for moment in [
        crate::verify::mark_passing(format!(
            "could not start the container hash: {}",
            std::io::Error::from_raw_os_error(libc::EAGAIN)
        )),
        crate::verify::mark_passing(
            "the container hash did not finish within 120s; treating as a failure".into(),
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
    let unreadable = aterm_update_core::sha256_file(std::path::Path::new("/nonexistent/container"));
    assert!(unreadable.is_err(), "{unreadable:?}");
    for verdict in [unreadable, Ok("cd".repeat(32))] {
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
    let (staging, end, reclaimed) = digest_of(Ok(manifest.sha256.to_ascii_uppercase()));
    assert!(end.is_none() && !reclaimed);
    assert!(FailedMark::read(&staging.failed()).is_none());
    let _ = std::fs::remove_dir_all(&staging.root);
}

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
    assert!(crate::manifest::FailedMark::read(&staging.failed()).is_none());
    assert_eq!(
        crate::health::Health::read(&staging.health()).total_failures(),
        0
    );
    let _ = std::fs::remove_dir_all(&staging.root);

    let staging = Staging::scratch("stage-quarantined");
    crate::manifest::quarantine_artifact(
        &staging,
        7,
        &manifest.sha256,
        aterm_durable::Flush::Durable,
    );
    let end = end_failed_stage(&staging, running, &manifest, "stage refused".into());
    assert_eq!(end, Ok(None));
    let _ = std::fs::remove_dir_all(&staging.root);

    let staging = Staging::scratch("stage-not-withdrawn");
    let end = end_failed_stage(&staging, running, &manifest, refused.into());
    assert_eq!(end, Err(refused.to_string()));
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn a_full_disk_is_charged_to_neither_the_release_nor_the_download_host() {
    use crate::manifest::FailedMark;
    use std::os::unix::process::ExitStatusExt as _;

    let manifest = candidate_manifest();
    let running = manifest.build_number - 1;
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
    for lane in ["unpack", "extract", "download"] {
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
        let memo = FailedMark::read(&staging.failed()).unwrap_or_else(|| panic!("{lane}: counted"));
        assert!(memo.moment, "{lane}: {memo:?}");
        assert!(
            memo.retry_in_secs(crate::install::unix_now_secs())
                <= *crate::manifest::MOMENT_BACKOFF_SECS.last().unwrap(),
            "{lane}: {memo:?}"
        );
        crate::health::backdate_streaks(&staging.health(), crate::PERSISTENT_SPAN);
        let h = crate::health::Health::read(&staging.health());
        assert_eq!(
            h.persistent_class(),
            Some(("stage", crate::PERSISTENT_AFTER)),
            "{lane}: a disk that stays full still escalates, as staging: {h:?}"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }

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
    let _ = std::fs::remove_dir_all(&staging.root);
    let quota = crate::install::unpack_failure(
        &volume.root,
        "ditto zip extract failed",
        std::process::ExitStatus::from_raw(1 << 8),
        "ditto: /x/zx-1/aterm.app/Contents/MacOS/aterm: Disc quota exceeded\n",
    );
    assert!(crate::is_passing_refusal(&quota), "{quota}");

    assert!(!aterm_update_core::download_error_is_local_write(
        "curl download failed (exit status: 56): curl: (56) Recv failure: Connection \
         reset by peer"
    ));
    let _ = std::fs::remove_dir_all(&volume.root);
}

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

    let first = check();
    assert!(!first.contains(STOPPED_STAGE_HOLDER), "{first}");
    for _ in 0..2 {
        StaleStageStamps::check_started();
        StaleStageStamps::check_ended(&path);
    }
    let again = check();
    assert!(!again.contains(STOPPED_STAGE_HOLDER), "{again}");
    assert_eq!(booked(), 0, "no false stopped-holder failure: {again}");
    let second = check();
    assert!(second.contains(STOPPED_STAGE_HOLDER), "{second}");
    assert_eq!(booked(), 1);
    assert!(is_stopped_stage_holder_refusal(
        "stage lock: another aterm process (the lock names pid 4242) holds the update \
         stage lock and has shown no sign of life across two checks"
    ));
    drop(held);
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn a_stage_lock_held_by_a_hung_check_in_a_running_aterm_is_booked() {
    use crate::checker_watch::{CheckerPhase, CheckerWatch};
    static WATCH: CheckerWatch = CheckerWatch::new();
    let wait = std::time::Duration::from_millis(400);

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

    let staging = Staging::scratch("stage-lock-alive-check");
    let (holder, release) = hold(&staging, false);
    for _ in 0..2 {
        let Err(error) = take_stage_lock(&staging, wait) else {
            panic!("the lock is held");
        };
        assert!(!error.contains(STOPPED_STAGE_HOLDER), "{error}");
    }
    assert_eq!(
        crate::health::Health::read(&staging.health()).total_failures(),
        0
    );
    release.send(()).unwrap();
    holder.join().unwrap();
    let _ = std::fs::remove_dir_all(&staging.root);

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

#[test]
fn a_stage_backoff_throttles_the_restage_never_an_already_staged_newer_build() {
    use crate::manifest::{FailedMark, RETRY_BACKOFF_SECS};

    let staging = Staging::scratch("stage-backoff-vs-apply");
    let manifest = candidate_manifest();
    let canonical_commit = manifest.commit.as_deref().unwrap().to_string();
    let running = manifest.build_number - 1;
    const NOW: u64 = 1_000_000;

    for _ in 0..4 {
        FailedMark::record_stage_failure(
            &staging.failed(),
            manifest.build_number,
            &manifest.sha256,
            NOW,
        );
    }

    let backoff =
        stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).expect("window open");
    assert_eq!(backoff.attempts, 4);
    assert_eq!(backoff.retry_in_secs, RETRY_BACKOFF_SECS[3]);
    assert!(backoff.applicable.is_none());

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
    let backoff =
        stage_backoff(&staging, &manifest, running, NOW, Asked::Schedule).expect("window open");
    let applicable = backoff.applicable.as_ref().expect("apply is not gated");
    assert_eq!(applicable.build_number, manifest.build_number);

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

    let past_deadline = NOW + RETRY_BACKOFF_SECS[3];
    assert!(
        stage_backoff(&staging, &manifest, running, past_deadline, Asked::Schedule).is_none(),
        "an expired window throttles nothing"
    );

    write_ready(
        &staging,
        manifest.build_number,
        &canonical_commit,
        &manifest.sha256,
    );
    assert!(publishable_stage_covers(&staging, &manifest));

    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn download_scratch_from_a_version_the_channel_moved_past_is_reclaimed() {
    let staging = Staging::scratch("download-sweep");

    let stale_part = staging.download.join("aterm-0.52.0-mac.zip.part");
    let stale_container = staging.download.join("aterm-0.53.0-mac.zip");
    let current_part = staging.download.join("aterm-0.54.0-mac.zip.part");
    for leftover in [&stale_part, &stale_container, &current_part] {
        std::fs::write(leftover, b"abandoned bytes").unwrap();
    }
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

    sweep_download_scratch(&staging);
    let _ = std::fs::remove_dir_all(&staging.root);
    sweep_download_scratch(&staging);
}

const MASTER_SEED_FIXTURE: [u8; 32] = [0xA7; 32];
const M3_SEED_FIXTURE: [u8; 32] = [0xB7; 32];
const OTHER_MASTER_FIXTURE: [u8; 32] = [0xA8; 32];

const ROSTER_NOW: i64 = 1_785_801_600;

fn attributed_manifest(machine_id: &str, roster_seq: u64) -> Vec<u8> {
    let mut text = String::from_utf8(manifest_bytes("0.10.0", 10, 0)).unwrap();
    text.push_str(&format!(
        "machine_id = {machine_id:?}\nroster_seq = {roster_seq}\n"
    ));
    text.into_bytes()
}

/// Everything the owner publishes for one release, with every placement under the
/// caller's control — which machine is on the roster, which is revoked, which one
/// signed, and at which generation.
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

fn serve_chain(c: &Chain, url: &str) -> Result<Vec<u8>, String> {
    match url.rsplit('/').next().unwrap_or_default() {
        APPCAST_ASSET => Ok(c.manifest.clone()),
        APPCAST_SIG_ASSET => Ok(c.manifest_sig.clone()),
        aterm_update_core::roster::ROSTER_ASSET => Ok(c.roster.clone()),
        aterm_update_core::roster::ROSTER_SIG_ASSET => Ok(c.roster_sig.clone()),
        other => Err(format!("HTTP 404: no such asset {other}")),
    }
}

fn run_chain(c: &Chain, masters: &[&str], floor_seq: u64, now_unix: i64) -> AuthoritativeFetch {
    fetch_authoritative_release(
        Some(web_release(&test_source(), WEB_TAG).unwrap()),
        &mut |url, _| serve_chain(c, url),
        &RosterPolicy {
            master_pubkeys: masters,
            floor_seq,
            now_unix,
            floor_refresh: None,
        },
    )
}

#[test]
fn a_release_whose_declared_machine_disagrees_with_the_signer_is_refused() {
    let lying = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m99", M3_SEED_FIXTURE),
        4,
        &MASTER_SEED_FIXTURE,
        4,
    );
    let masters = [lying.master_pub.as_str()];
    let fetched = run_chain(&lying, &masters, 0, ROSTER_NOW);
    assert!(
        fetched.selected.is_none() && fetched.manifest_rejected,
        "a signature cannot be relabelled onto another machine's identity"
    );
}

#[test]
fn an_appcast_of_another_version_under_the_tag_is_refused() {
    let signed = |version: &str| {
        let mut c = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        let text = String::from_utf8(manifest_bytes(version, 10, 7))
            .unwrap()
            .replace(&format!("aterm-{version}.dmg"), "aterm-0.10.0.dmg");
        c.manifest = (text + "machine_id = \"m3\"\nroster_seq = 4\n").into_bytes();
        c.manifest_sig = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE)
            .unwrap()
            .sign(&c.manifest)
            .as_ref()
            .to_vec();
        c
    };
    let other = signed("0.9.0");
    let masters = [other.master_pub.as_str()];
    let refused = run_chain(&other, &masters, 0, ROSTER_NOW);
    assert!(refused.selected.is_none() && refused.manifest_rejected);
    let own = run_chain(&signed("0.10.0"), &masters, 0, ROSTER_NOW);
    assert!(own.selected.is_some());
    let observation = own.floor_observation();
    assert_eq!(
        (observation.min_build, observation.machine),
        (7, Some("m3"))
    );
}

#[test]
fn a_selected_release_unbound_to_its_tag_or_durably_revoked_is_refused() {
    let bound = tag_url(WEB_TAG, "aterm-0.10.0.dmg");
    let revoking = crate::manifest::Floor {
        revoked_machines: vec!["m3".into()],
        ..crate::manifest::Floor::default()
    };
    let rows = [
        (
            Some(bound.as_str()),
            crate::manifest::Floor::default(),
            true,
        ),
        (None, crate::manifest::Floor::default(), false),
        (Some(bound.as_str()), revoking, false),
    ];
    for (url, floor, stands) in rows {
        let channel = web_channel(url);
        let masters = [channel.master_pub.as_str()];
        let mut fetched = fetch_authoritative_release(
            Some(web_release(&test_source(), WEB_TAG).unwrap()),
            &mut |url, _| channel.serve(url),
            &channel.policy(&masters),
        );
        assert!(fetched.selected.is_some(), "the chain selects it");
        fetched.refuse_unbound_or_revoked(&test_source(), &floor, WEB_TAG);
        assert_eq!(
            (fetched.selected.is_some(), fetched.manifest_rejected),
            (stands, !stands),
            "url {url:?}, revoked {:?}",
            floor.revoked_machines
        );
    }
}

#[test]
fn the_roster_floor_ratchets_on_observation_not_on_acceptance() {
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
    let saw_revocation = run_chain(&revoking, &masters, 9, ROSTER_NOW);
    assert!(saw_revocation.selected.is_none() && saw_revocation.manifest_rejected);
    assert_eq!(
        saw_revocation.observed_roster_seq,
        Some(10),
        "observing the revoking generation must be reported for the durable ratchet"
    );
    let observation = saw_revocation.floor_observation();
    assert_eq!(
        (
            observation.roster_seq,
            observation.revoked,
            observation.machine
        ),
        (10, &["m3".to_string()][..], None)
    );

    let pre_revocation = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        9,
        &MASTER_SEED_FIXTURE,
        9,
    );
    let floor_after_observation = saw_revocation.observed_roster_seq.unwrap();
    let replayed = run_chain(
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

    let accepted = run_chain(&pre_revocation, &masters, 9, ROSTER_NOW);
    assert!(accepted.selected.is_some());
    assert_eq!(accepted.observed_roster_seq, Some(9));
}

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
    let run_with = |snapshot: u64, refreshed_floor: u64| {
        let refresh = move || refreshed_floor;
        fetch_authoritative_release(
            Some(web_release(&test_source(), WEB_TAG).unwrap()),
            &mut |url, _| serve_chain(&c, url),
            &RosterPolicy {
                master_pubkeys: &masters,
                floor_seq: snapshot,
                now_unix: ROSTER_NOW,
                floor_refresh: Some(&refresh),
            },
        )
    };
    let run_with_refresh = |refreshed_floor: u64| run_with(0, refreshed_floor);
    let raced = run_with_refresh(10);
    assert!(
        raced.selected.is_none() && raced.manifest_rejected,
        "admission must honour the re-read floor, not the pre-download snapshot"
    );
    assert_eq!(raced.observed_roster_seq, None);
    let quiet = run_with_refresh(9);
    assert!(quiet.selected.is_some());
    assert_eq!(quiet.observed_roster_seq, Some(9));
    let unread = run_with(10, 0);
    assert!(unread.selected.is_none() && unread.manifest_rejected);
}

#[test]
fn a_revocation_leaves_an_unattributed_stage_alone() {
    let revoked = vec!["m11".to_string()];
    assert!(revocation_withdraws_stage(&revoked, Some("m11")));
    assert!(!revocation_withdraws_stage(&revoked, None));
}

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

#[test]
fn a_revoked_machines_floors_are_withdrawn_so_the_next_genuine_release_is_not_held() {
    let staging = Staging::scratch("floor-revocation");
    let floor = || crate::manifest::Floor::read(&staging.floor());
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
    assert!(floor_hold(800, after.min_build, after.high_water).is_some());
    assert!(floor_hold(1_400, after.min_build, after.high_water).is_some());
    assert!(Ready::read_published_bundle(&staging).is_none());

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
        refusal.as_deref().is_some_and(crate::is_passing_refusal),
        "a retired stage is refused, as the stage's state (a moment): {refusal:?}"
    );
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
    std::fs::remove_dir_all(&staging.staged_app).unwrap();
    let refusal = crate::install::handoff_cached_pass_refusal_at(&staging, 21, commit);
    assert!(
        refusal.as_deref().is_some_and(crate::is_passing_refusal),
        "{refusal:?}"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

fn publish_stage_confirmed_at(staging: &Staging, machine: &str, confirmed: u64) -> Ready {
    publish_attributed_stage(staging, 20, machine);
    let mut ready = Ready::read(&staging.ready).unwrap();
    ready.roster_confirmed_at = Some(aterm_types::rfc3339::format_rfc3339(confirmed));
    std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
    ready
}

#[test]
fn the_check_after_a_launch_confirms_a_stale_stage() {
    let now = crate::install::unix_now_secs();
    let stamp = aterm_types::rfc3339::format_rfc3339(now);
    let observe = |staging: &Staging, roster_seq: u64| {
        observe_floor_and_withdraw_at(
            staging,
            10,
            crate::manifest::Floor::read(&staging.floor()),
            &crate::manifest::FloorObservation {
                roster_seq,
                ..crate::manifest::FloorObservation::default()
            },
            &stamp,
        )
        .unwrap()
    };
    let staging = Staging::scratch("roster-reconfirm");
    let stale = publish_stage_confirmed_at(
        &staging,
        "m11",
        now - 2 * crate::manifest::ROSTER_FRESHNESS_SECS,
    );
    assert!(
        stage_owes_roster_reconfirmation(&staging, 10, now, true),
        "the check re-reads an unchanged head's roster for it"
    );
    observe(&staging, 0);
    assert_eq!(
        Ready::read(&staging.ready).unwrap().roster_confirmed_at,
        stale.roster_confirmed_at,
        "a check that admitted no roster confirms nothing"
    );
    observe(&staging, 9);
    let confirmed = Ready::read_publishable(&staging).expect("kept and published");
    assert_eq!(
        confirmed.roster_confirmed_at.as_deref(),
        Some(stamp.as_str())
    );
    assert!(!stage_owes_roster_reconfirmation(&staging, 10, now, true));
    let _ = std::fs::remove_dir_all(&staging.root);
}

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
    publish_stage_confirmed_at(&staging, "m3", now - 600);
    let (outcome, heads) = acquire_web(&staging, Some(WEB_TAG), || redirect_to(WEB_TAG));
    assert_eq!(heads.len(), 1);
    assert!(
        matches!(outcome, Ok(Acquisition::UpToDate { .. })),
        "{outcome:?}"
    );
    assert!(!stage_owes_roster_reconfirmation(
        &staging,
        20,
        now + 86_400 * 2,
        true
    ));
    assert!(!stage_owes_roster_reconfirmation(
        &staging,
        10,
        now + 86_400 * 2,
        false
    ));
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn roster_freshness_conformance() {
    use aterm_spec::interp::State;
    let model = aterm_models_update::native_update_roster_freshness_model();
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .expect("a model constant")
    };
    let window = constant("F");
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
    let mut swap_runs = std::collections::BTreeMap::new();
    let mut check_runs = std::collections::BTreeMap::new();
    let (mut swaps, mut checks, mut floor_alone_late) = (0, 0, None);
    for state in reachable
        .iter()
        .filter(|state| state["staged"] == 1 && state["swapped"] == 0)
    {
        let (admitted, disk, floor_alone) = *swap_runs
            .entry((state["conf"], state["now"]))
            .or_insert_with(|| {
                let (staging, ready) = lay_out(state["conf"]);
                let admitted = crate::install::stage_admission_gates(
                    &staging,
                    10,
                    &ready,
                    None,
                    at(state["now"]),
                )
                .is_none();
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
    let stamped = Ready::read_published_bundle(&staging).unwrap();
    assert_eq!(
        crate::install::stage_admission_gates(&staging, 10, &stamped, None, at(blind["now"])),
        None
    );
    let swapped = project(read_disk(&staging), &blind, Some(late(&blind)));
    assert!(!model.check_invariant("NoSwapPastTheWindow", &swapped));
}

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
    assert_eq!(applicable_stage(&staging, 10), Some("0.0.20".to_string()));
    let backoff = stage_backoff(&staging, &manifest, 10, now, Asked::Schedule).expect("suppressed");
    assert!(backoff.applicable.is_some());

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
    let floor = crate::manifest::Floor::read(&staging.floor());
    observe_floor_and_withdraw(
        &staging,
        10,
        floor,
        &crate::manifest::FloorObservation::default(),
    )
    .unwrap();
    assert!(Ready::read_published_bundle(&staging).is_none(), "retired");

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

/// Repeated `floor_snapshot` read failures book `pipeline` failures and surface
/// a persistent failure notice naming the unreadable floor file at
/// `PERSISTENT_AFTER`, whereas a readable or absent floor books nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_floor_that_stays_unreadable_is_booked_until_it_is_said() {
    use std::os::unix::fs::PermissionsExt as _;
    let staging = Staging::scratch("floor-unreadable");
    let absent = floor_snapshot(&staging).expect("no floor is the first-contact default");
    assert_eq!(absent.min_build, 0);
    std::fs::write(staging.floor(), "min_build = 500\nroster_seq = 10\n").unwrap();
    std::fs::set_permissions(staging.floor(), std::fs::Permissions::from_mode(0o000)).unwrap();
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
        crate::health::backdate_streaks(&staging.health(), crate::PERSISTENT_SPAN);
        let h = crate::health::Health::read(&staging.health());
        assert_eq!(
            h.persistent_class(),
            Some(("pipeline", crate::PERSISTENT_AFTER)),
            "booked, so it can escalate: {h:?}"
        );
        let (_, body) = crate::persistent_failure_notice("pipeline", h.pipeline_failures, &h, 1);
        assert!(body.contains(&floor), "the notice names the file: {body}");
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
    assert!(!roster_authority_superseded(Some(9), 9));
    assert!(roster_authority_superseded(Some(9), 10));
    assert!(!roster_authority_superseded(None, u64::MAX));
}

#[test]
fn a_stale_anchor_names_the_reinstall_and_a_publisher_fault_does_not() {
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
    let forged = run_chain(&wrong_master, &masters, 0, ROSTER_NOW);
    let reason = forged.rejection_reason.as_deref().expect("reason");
    assert!(
        forged.selected.is_none() && forged.manifest_rejected,
        "a master this build does not pin authorizes nothing: {reason}"
    );
    assert!(
        is_stale_anchor_refusal(reason),
        "an unverifiable roster is the stranded client's arm: {reason}"
    );

    let lapsed = run_chain(&good, &masters, 0, 1_900_000_000);
    let reason = lapsed.rejection_reason.as_deref().expect("reason");
    assert!(
        lapsed.manifest_rejected && !is_stale_anchor_refusal(reason),
        "a lapsed roster is the publisher's to refresh, not a reinstall: {reason}"
    );
    assert_eq!(
        lapsed.observed_roster_seq, None,
        "a stale roster failed admission: not an observation, moves no floor"
    );
    let replayed = run_chain(&good, &masters, 5, ROSTER_NOW);
    let reason = replayed.rejection_reason.as_deref().expect("reason");
    assert!(
        replayed.manifest_rejected && !is_stale_anchor_refusal(reason),
        "a rolled-back roster is a replay, not a stale anchor: {reason}"
    );
    let mut newer = good.clone();
    let text = String::from_utf8(newer.roster).unwrap();
    assert_eq!(text.matches("schema = 1\n").count(), 1, "{text}");
    newer.roster = text.replace("schema = 1\n", "schema = 2\n").into_bytes();
    newer.roster_sig = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE)
        .unwrap()
        .sign(&newer.roster)
        .as_ref()
        .to_vec();
    let unreadable = run_chain(&newer, &masters, 0, ROSTER_NOW);
    assert!(
        unreadable.manifest_rejected && !unreadable.appcast_fetch_error,
        "{:?}",
        unreadable.rejection_reason
    );
    let accepted = run_chain(&good, &masters, 0, ROSTER_NOW);
    assert!(accepted.selected.is_some() && accepted.rejection_reason.is_none());
}

#[test]
fn a_signature_that_is_not_signature_shaped_is_a_delivery_fault_not_a_stale_anchor() {
    let base = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &["m11"],
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
        let fetched = run_chain(c, &masters, 0, ROSTER_NOW);
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
    let fetched = run_chain(&appcast_sig_blocked, &masters, 0, ROSTER_NOW);
    assert_eq!(fetched.observed_roster_seq, Some(4));
    assert_eq!(fetched.observed_revocations, vec!["m11".to_string()]);
    let fetched = run_chain(&roster_sig_blocked, &masters, 0, ROSTER_NOW);
    assert_eq!(
        fetched.observed_roster_seq, None,
        "no roster, no observation"
    );
}

#[test]
fn the_roster_generation_must_agree_between_the_roster_and_the_signed_manifest() {
    let lying = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        6,
        &MASTER_SEED_FIXTURE,
        7,
    );
    let masters = [lying.master_pub.as_str()];
    let refused = run_chain(&lying, &masters, 0, ROSTER_NOW);
    assert!(
        refused.selected.is_none() && refused.manifest_rejected,
        "an appcast may not name a roster generation newer than the one that \
         authorized it"
    );
    let redressed = chain(
        &[("m3", M3_SEED_FIXTURE)],
        &[],
        ("m3", M3_SEED_FIXTURE),
        6,
        &MASTER_SEED_FIXTURE,
        5,
    );
    let admitted = run_chain(&redressed, &masters, 0, ROSTER_NOW);
    assert!(
        admitted.selected.is_some(),
        "a newer roster paired with an older release is the post-join steady state"
    );
    assert_eq!(admitted.observed_roster_seq, Some(6));
}

const WEB_TAG: &str = "v0.10.0";

fn tag_url(tag: &str, name: &str) -> String {
    aterm_update_core::cdn::release_download_url("alabsystems", "aterm", tag, name).unwrap()
}

fn tag_prefix(tag: &str) -> String {
    let url = tag_url(tag, "x");
    url[..url.len() - 1].to_string()
}

fn evergreen_url() -> String {
    aterm_update_core::pointer::latest_download_url("alabsystems", "aterm", APPCAST_ASSET).unwrap()
}

fn redirect_to(tag: &str) -> Result<HeadAnswer, HttpError> {
    Ok(HeadAnswer {
        code: 302,
        location: Some(tag_url(tag, APPCAST_ASSET)),
    })
}

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
    fn policy<'a>(&self, masters: &'a [&'a str]) -> RosterPolicy<'a> {
        RosterPolicy {
            master_pubkeys: masters,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        }
    }

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

const WEB_BUILD: u64 = 5;

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

#[test]
fn an_unchanged_web_head_refetches_a_release_whose_stage_is_lost() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("web-lost-stage");
    let source = test_source();
    let commit = "0123456789abcdef0123456789abcdef01234567";
    write_ready(&staging, 10, commit, &"ab".repeat(32));
    write_bundle_identity(&staging, 10, commit);
    std::fs::write(
        &staging.status,
        format!(
            "schema = 1\noutcome = \"verified stage published\"\ncurrent_build = \
             {WEB_BUILD}\nlatest_tag = {WEB_TAG:?}\nlatest_source = \"alabsystems/aterm\"\n\
             latest_authorized_build = {WEB_BUILD}\nlatest_release_build = 10\n"
        ),
    )
    .unwrap();
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads, vec![evergreen_url()]);
    assert!(
        matches!(outcome, Ok(Acquisition::UpToDate { .. })),
        "{outcome:?}"
    );

    std::fs::remove_file(&staging.ready).unwrap();
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads, vec![evergreen_url()]);
    assert!(
        matches!(outcome, Ok(Acquisition::Proceed(_))),
        "the unchanged head must retry the lost stage: {outcome:?}"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn every_web_lane_failure_path_ends_without_an_api_request() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("failures");
    crate::status::clear_check_note();

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
    assert!(
        !staging.health().exists(),
        "a standing state is not a fault"
    );
    note_readable(&test_source());
    assert!(!crate::unreadable::is_stranded());

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
        assert!(!staging.health().exists(), "{code}: weather is not a fault");
    }

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
        assert!(
            !error.contains("someone/aterm"),
            "{label}: a refusal never echoes the server's string: {error}"
        );
        assert_eq!(
            crate::health::Health::read(&staging.health()).network_failures,
            before + 1,
            "{label}: booked as network"
        );
    }
    note_readable(&test_source());
    assert!(!rate_limited());
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn the_pointers_tag_and_the_manifest_version_must_agree() {
    let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
    let masters = [channel.master_pub.as_str()];
    let candidate = web_release(&test_source(), "v0.11.0").unwrap();
    let mut gets = Vec::new();
    let mut download = |url: &str, _max: u64| {
        gets.push(url.to_string());
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

#[test]
fn a_roster_that_cannot_be_fetched_on_the_web_lane_refuses_as_transport() {
    let source = test_source();
    let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
    let masters = [channel.master_pub.as_str()];
    for missing in [
        aterm_update_core::roster::ROSTER_ASSET,
        aterm_update_core::roster::ROSTER_SIG_ASSET,
    ] {
        let candidate = web_release(&source, WEB_TAG).unwrap();
        let mut gets = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            if url.ends_with(missing) {
                return Err("HTTP 404: the release does not carry this asset".to_string());
            }
            channel.serve(url)
        };
        let fetched =
            fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
        assert!(fetched.selected.is_none(), "{missing}: refused");
        assert!(
            fetched.appcast_fetch_error,
            "{missing}: transport, not a manifest verdict"
        );
        assert!(!fetched.manifest_rejected, "{missing}");
        assert!(!fetched.asset_fetch_rate_limited, "{missing}");
        assert!(fetched.attribution.is_none(), "{missing}");
        assert!(
            gets.iter().all(|u| !aterm_update_core::cdn::is_api_host(u)),
            "{gets:?}"
        );
    }
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

#[test]
fn the_shortcut_is_bound_to_the_recording_build_and_source_and_cleared_by_retirement() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("shortcut-binding");
    let source = test_source();
    write_ledger(&staging, "v0.9.0", WEB_BUILD, "alabsystems/aterm");
    let (outcome, _) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert!(
        matches!(outcome, Ok(Acquisition::Proceed(_))),
        "{outcome:?}"
    );
    for other in [WEB_BUILD - 1, WEB_BUILD + 1] {
        write_ledger(&staging, WEB_TAG, other, "alabsystems/aterm");
        let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert_eq!(heads, vec![evergreen_url()]);
        assert!(
            matches!(outcome, Ok(Acquisition::Proceed(_))),
            "build {other}: {outcome:?}"
        );
    }
    write_ledger(&staging, WEB_TAG, WEB_BUILD, "example/mirror");
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads.len(), 1);
    assert!(
        matches!(outcome, Ok(Acquisition::Proceed(_))),
        "{outcome:?}"
    );
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
    crate::status::clear_check_note();
    crate::status::set_latest_tag(WEB_TAG, &source, WEB_BUILD, WEB_BUILD);
    crate::status::record(&staging, WEB_BUILD, "staged 0.10.0 (build 10)");
    let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
    assert_eq!(heads.len(), 1);
    assert!(
        matches!(outcome, Ok(Acquisition::UpToDate { .. })),
        "{outcome:?}"
    );
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
        web_container_url_agrees(&mirror, tag, &with_url(Some(&bad))).expect_err(why);
    }
    web_container_url_agrees(&mirror, tag, &with_url(None)).expect_err("no container `url`");
}

#[test]
fn a_published_release_verifies_under_the_committed_master_alone() {
    const APPCAST: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-appcast.toml");
    const APPCAST_SIG: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-appcast.toml.sig");
    const ROSTER: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-machines.toml");
    const ROSTER_SIG: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-machines.toml.sig");
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
}

#[test]
fn a_source_only_head_is_never_the_authorized_tag() {
    let _guard = crate::STRANDED_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let staging = Staging::scratch("non-app-head");
    crate::status::clear_check_note();
    let fetched = fetch_authoritative_release(
        Some(web_release(&test_source(), WEB_TAG).unwrap()),
        &mut |url: &str, _max: u64| Err(format!("HTTP 404: no such asset {url}")),
        &RosterPolicy {
            master_pubkeys: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
            floor_seq: 0,
            now_unix: ROSTER_NOW,
            floor_refresh: None,
        },
    );
    assert!(fetched.selected.is_none() && fetched.appcast_missing);
    record_head_without_app(&staging, WEB_BUILD, WEB_TAG);
    assert!(
        crate::status::latest_tag(&staging, WEB_BUILD, &test_source()).is_none(),
        "a head with no app manifest is never the authorized tag"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

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
    assert_eq!(
        crate::status::latest_tag(&staging, WEB_BUILD, &source).as_deref(),
        Some("v0.9.0"),
        "the head is never authorized, and the ledger keeps what it had"
    );
    assert_eq!(
        crate::health::Health::read(&staging.health()).acquisition_failures(),
        0
    );
    assert!(!crate::unreadable::is_stranded(), "not a strand");
    assert_eq!(head_in_flight(), None, "not an app build in flight");
    let _ = std::fs::remove_dir_all(&staging.root);
}

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
    write_ledger(&staging, "v0.93.0", 42, "alabsystems/aterm");
    assert!(!latched(42, "v0.92.0"), "the recording build");
    assert!(!latched(41, "v0.92.0"), "another build, the same answer");
    assert!(latched(42, "v0.94.0"));
    assert!(latched(41, "v0.94.0"));
    assert!(!app_build_in_flight("v0.93.0", Some("v0.93.0")));
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
    write_ledger(&staging, "v0.93.0", 42, "someone/else");
    assert!(latched(41, "v0.92.0"));
    let _ = std::fs::remove_dir_all(&staging.root);
}

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
    assert_eq!(stamp(&healthy, true, v92), None);
    assert_eq!(stamp(&healthy, false, v92), Some(("v0.92.0".into(), 1)));
    let before = std::fs::read(crate::check_receipt::path(&staging)).unwrap();
    assert_eq!(stamp(&Err("dns".into()), false, v92), None);
    assert_eq!(
        std::fs::read(crate::check_receipt::path(&staging)).unwrap(),
        before
    );
    assert_eq!(stamp(&healthy, false, v92), Some(("v0.92.0".into(), 2)));
    assert!(check_leaves_a_receipt(
        &Err("connection reset".into()),
        true
    ));
    assert_eq!(
        stamp(&healthy, false, Some("v0.93.0")),
        Some(("v0.93.0".into(), 1))
    );
    assert_eq!(stamp(&healthy, false, None), None);
    assert_eq!(
        stamp(&healthy, false, Some("v0.93.0")),
        Some(("v0.93.0".into(), 1))
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// The app-container lane end to end, over real curl (`github_e2e_tests.rs`).
#[path = "github_e2e_tests.rs"]
mod e2e;
