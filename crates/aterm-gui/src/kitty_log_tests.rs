// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_effects::cat_glyphs_gen::CatGlyphId;
use aterm_models_core::kitty_pin_merge_model;
use aterm_models_effects::{
    kitty_collectibles_model, kitty_flush_worker_model, kitty_sidecar_durability_model,
};
use aterm_spec::derive::Model;
use aterm_spec::verify;
use std::collections::BTreeMap;

fn tmp(name: &str) -> PathBuf {
    let d = crate::test_scratch::process_root("aterm-kitty-log-test-").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join(KITTY_LOG_FILE)
}

fn sighting(ident: u64) -> KittySighting {
    KittySighting {
        kitty_type: KittyType::HeadPeek,
        magic: KittyMagic::None,
        shown_as: KittyShownAs::Cat,
        langs: LangSet::EMPTY,
        traits: TRAIT_SHY,
        look: KittyLook::default(),
        ident,
    }
}

fn write_transitional_embedded(path: &Path, log: &KittyLog) {
    std::fs::write(
        path,
        aterm_toml::to_string(log).expect("serialize legacy ledger"),
    )
    .expect("write transitional ledger");
}

fn write_precollectibles_rewrite(path: &Path, log: &KittyLog) {
    let mut old = log.clone();
    old.collectibles.clear();
    old.write(path);
}

fn exact_limit_toml(prefix: &str) -> String {
    assert!(prefix.len() < MAX_KITTY_LEDGER_BYTES);
    let mut text = String::with_capacity(MAX_KITTY_LEDGER_BYTES);
    text.push_str(prefix);
    text.push('#');
    text.push_str(&"x".repeat(MAX_KITTY_LEDGER_BYTES - text.len()));
    assert_eq!(text.len(), MAX_KITTY_LEDGER_BYTES);
    text
}

fn host_startup_and_exit(config_path: PathBuf) -> (u64, usize) {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::load(Some(config_path));
        host.await_initial_load();
        let startup = (host.log().sightings, host.log().collectibles.len());
        host.observe(91, [sighting(91)], lex, Instant::now(), true);
        host.flush_exit();
        let _ = done_tx.send(startup);
    });
    done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("host startup plus background flush/exit must be bounded")
}

#[test]
fn store_round_trips_through_toml() {
    let p = tmp("roundtrip");
    let lex = Lexicon::builtin();
    let mut log = KittyLog::default();
    log.record(&sighting(1), lex, "2026-07-01T10:00:00Z");
    let crowned = KittySighting {
        traits: TRAIT_SHY | TRAIT_CROWN,
        ..sighting(2)
    };
    log.record(&crowned, lex, "2026-07-02T10:00:00Z");
    log.write(&p);
    assert!(KittyLog::write_collectible_store(
        &collectibles_path(&p),
        &log.collectibles,
    ));
    let back = KittyLog::read(&p);
    assert_eq!(back, log, "legacy mirror write → read is identity");
    assert!(
        std::fs::read_to_string(&p)
            .expect("legacy ledger")
            .contains("collectibles"),
        "collectible-aware rollback must see the embedded mirror"
    );
    let full = KittyLog::read_with_sidecar(&p);
    assert_eq!(full, log, "combined legacy + sidecar read is identity");
    assert_eq!(back.sightings, 2);
    assert_eq!(back.trait_shy, 2);
    assert_eq!(back.accessory_crown, 1, "v3 accessory counter persists");
    assert_eq!(back.shown_cat, 2);
    assert_eq!(back.entries.len(), 1, "same (type, magic, lang) cell");
    assert_eq!(back.entries[0].count, 2);
    assert_eq!(back.entries[0].first_seen, "2026-07-01T10:00:00Z");
    assert_eq!(back.entries[0].last_seen, "2026-07-02T10:00:00Z");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn corrupt_or_absent_file_reads_empty() {
    let p = tmp("corrupt");
    assert!(KittyLog::read(&p).is_empty(), "absent ⇒ empty");
    std::fs::write(&p, "not = [valid").unwrap();
    assert!(KittyLog::read(&p).is_empty(), "corrupt ⇒ empty");
    std::fs::write(&p, [0xff, 0xfe]).unwrap();
    assert!(KittyLog::read(&p).is_empty(), "non-UTF-8 ⇒ empty");
    std::fs::write(&p, "sightings = 3\nfuture_key = true\n").unwrap();
    let l = KittyLog::read(&p);
    assert_eq!(l.sightings, 3, "known fields survive unknown keys");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn ledger_cap_accepts_exact_limit_and_rejects_oversized_sparse_files() {
    let legacy = tmp("bounded-legacy");
    std::fs::write(&legacy, exact_limit_toml("sightings = 7\n"))
        .expect("write exact-limit legacy ledger");
    assert_eq!(
        KittyLog::read(&legacy).sightings,
        7,
        "the documented limit is inclusive"
    );

    let sidecar = collectibles_path(&legacy);
    std::fs::write(
        &sidecar,
        exact_limit_toml("collectibles = []\nlegacy_mirror = []\n"),
    )
    .expect("write exact-limit sidecar");
    assert_eq!(
        KittyLog::read_collectible_store(&sidecar),
        Some(KittyCollectibleStore {
            collectibles: Vec::new(),
            legacy_mirror: Some(Vec::new()),
        }),
        "the same inclusive boundary applies to the sidecar"
    );

    let oversized_legacy = tmp("oversized-sparse-legacy");
    std::fs::File::create(&oversized_legacy)
        .and_then(|file| file.set_len((MAX_KITTY_LEDGER_BYTES + 1) as u64))
        .expect("create oversized sparse legacy ledger");
    assert!(
        KittyLog::read(&oversized_legacy).is_empty(),
        "oversized legacy ledger fails open without reading its body"
    );

    let oversized_sidecar = collectibles_path(&oversized_legacy);
    std::fs::File::create(&oversized_sidecar)
        .and_then(|file| file.set_len((MAX_KITTY_LEDGER_BYTES + 1) as u64))
        .expect("create oversized sparse collectible sidecar");
    assert_eq!(
        KittyLog::read_collectible_store(&oversized_sidecar),
        None,
        "oversized sidecar is the absent/default state"
    );

    let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    let _ = std::fs::remove_dir_all(oversized_legacy.parent().unwrap());
}

#[cfg(unix)]
fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("FIFO path CString");
    // SAFETY: `path` is a live NUL-terminated pathname and `mkfifo` retains no pointer after returning.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

/// A hostile target planted at the legacy ledger, the collectible sidecar
/// or the lock — a writerless FIFO, or a final symlink to a victim that a
/// followed read would admit — is admitted as empty (or fails the lock at
/// once): startup and exit stay bounded, the target is neither followed
/// nor replaced, the victim is never written through, and an unsafe
/// sidecar or an untaken lock gates the mirror write.
#[cfg(unix)]
#[test]
fn a_hostile_target_at_any_ledger_path_is_not_followed_and_cannot_block_startup_or_exit() {
    use std::os::unix::fs::{FileTypeExt as _, symlink};

    let mut seeded = KittyLog::default();
    seeded.record(&sighting(1), Lexicon::builtin(), "2026-07-01T00:00:00Z");
    let store_text = aterm_toml::to_string(&KittyCollectibleStore::mirrored(&seeded.collectibles))
        .expect("serialize sidecar victim");
    for (case, gates_legacy) in [("legacy", false), ("sidecar", true), ("lock", true)] {
        for fifo in [true, false] {
            let legacy = tmp(&format!("hostile-{case}-{fifo}"));
            let target = match case {
                "legacy" => legacy.clone(),
                "sidecar" => collectibles_path(&legacy),
                _ => legacy.with_extension("toml.lock"),
            };
            let victim = legacy.with_file_name(format!("{case}-victim.toml"));
            let victim_text = if case == "sidecar" {
                store_text.clone()
            } else {
                "sightings = 99\n".to_string()
            };
            if fifo {
                make_fifo(&target);
            } else {
                std::fs::write(&victim, &victim_text).expect("write victim");
                symlink(&victim, &target).expect("plant final symlink");
            }
            assert_eq!(
                host_startup_and_exit(legacy.with_file_name("aterm.toml")),
                (0, 0),
                "{case} fifo={fifo}: bounded, and admitted as empty"
            );
            let kind = std::fs::symlink_metadata(&target)
                .expect("the hostile target remains")
                .file_type();
            assert!(
                if fifo {
                    kind.is_fifo()
                } else {
                    kind.is_symlink()
                },
                "{case} fifo={fifo}: best-effort flush must not replace a hostile target"
            );
            if !fifo {
                assert_eq!(
                    std::fs::read_to_string(&victim).expect("read victim"),
                    victim_text,
                    "{case}: never written through"
                );
            }
            if gates_legacy {
                assert!(
                    !legacy.exists(),
                    "{case} fifo={fifo}: the mirror write is gated"
                );
            }
            let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
        }
    }
}

#[test]
fn held_sibling_lock_never_parks_flush_exit() {
    let legacy = tmp("held-lock");
    let held = try_lock(&legacy).expect("hold sibling lock");

    assert_eq!(
        host_startup_and_exit(legacy.with_file_name("aterm.toml")),
        (0, 0),
        "startup plus exit stays bounded while another process owns the lock"
    );
    assert!(!legacy.exists(), "a contended flush must not race unlocked");
    drop(held);
    let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
}

/// Both ledger locks are free when a flush ends, even while a child forked under
/// them has yet to exec.
#[cfg(unix)]
#[test]
fn a_flush_frees_both_locks_while_a_child_forked_under_them_has_yet_to_exec() {
    let legacy = tmp("fork-window");
    let lex = Lexicon::builtin();
    let mut first = KittyLog::default();
    first.record(&sighting(21), lex, "2026-07-21T00:00:00Z");
    let armed = crate::parked_fork::arm_at(2);
    assert!(KittyLog::flush_merge(&legacy, &first), "uncontended flush");
    let child = armed.child();
    assert!(child.still_parked(), "forked under the first flush's locks");

    let mut second = KittyLog::default();
    second.record(&sighting(22), lex, "2026-07-21T00:00:01Z");
    assert!(
        KittyLog::flush_merge(&legacy, &second),
        "the next single-try flush takes both locks"
    );
    assert!(child.still_parked(), "…while the child had yet to exec");
    child.release();
    assert_eq!(KittyLog::read_with_sidecar(&legacy).sightings, 2);
    let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
}

/// A refused sidecar lock drops the legacy lock by `LOCK_UN`, so a child forked
/// while it was held does not keep it and the next pair take succeeds.
#[cfg(unix)]
#[test]
fn a_refused_sidecar_frees_the_legacy_lock_while_a_child_forked_under_it_has_yet_to_exec() {
    let legacy = tmp("fork-window-half");
    let sidecar = collectibles_path(&legacy);
    let peer =
        HeldAdvisoryLock::try_lock(open_lock(&sidecar.with_extension("toml.lock")).expect("open"))
            .expect("a peer holds the sidecar lock");
    let armed = crate::parked_fork::arm_at(1);
    assert!(lock_pair(&legacy, &sidecar).is_none(), "sidecar refused");
    let child = armed.child();
    drop(peer);
    assert!(child.still_parked(), "forked under the legacy lock");
    assert!(
        lock_pair(&legacy, &sidecar).is_some(),
        "the next pair take finds the legacy lock free"
    );
    assert!(child.still_parked(), "…while the child had yet to exec");
    child.release();
    let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
}

#[test]
fn finite_exit_lock_retries_conform_and_reject_dropped_pending_batch() {
    let model = kitty_flush_worker_model();
    let project = |accepted,
                   normal_lane,
                   host_tail,
                   exit_lane,
                   pending,
                   persisted,
                   exiting,
                   retries,
                   joined| {
        [
            ("accepted", accepted),
            ("normal_lane", normal_lane),
            ("host_tail", host_tail),
            ("exit_lane", exit_lane),
            ("pending", pending),
            ("persisted", persisted),
            ("exiting", exiting),
            ("retries", retries),
            ("joined", joined),
            ("stalled", 0),
            ("deadline", 0),
            ("detached", 0),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
    };
    let validate = |action: &str,
                    before: &BTreeMap<&'static str, i64>,
                    after: &BTreeMap<&'static str, i64>| {
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            "real Kitty Log finite exit flush",
        );
        assert!(ok, "model rejected {action}: {why}");
    };

    let initial = model.init_state();
    let queued = project(1, 1, 0, 0, 0, 0, 0, 0, 0);
    validate("QueueNormal", &initial, &queued);
    let drained = project(1, 0, 0, 0, 1, 0, 0, 0, 0);
    validate("DrainNormal", &queued, &drained);
    assert!(
        !model.action_enabled("Contend", &drained),
        "active-runtime contention must not consume the fresh exit retry budget"
    );
    let exiting = project(1, 0, 0, 0, 1, 0, 1, 0, 0);
    validate("BeginExit", &drained, &exiting);

    let legacy = tmp("exit-retry-conformance");
    let held = try_lock(&legacy).expect("hold sibling lock");
    let mut delta = KittyLog::default();
    delta.record(&sighting(8), Lexicon::builtin(), "2026-07-21T00:00:00Z");
    let result = flush_pending_at_exit(&legacy, &delta);
    assert_eq!(
        result,
        ExitFlushResult {
            persisted: false,
            attempts: EXIT_LOCK_RETRIES,
        },
        "the shipping exit helper consumes exactly the finite retry budget"
    );
    assert!(
        !legacy.exists(),
        "contention must retain rather than race the batch"
    );

    let mut before = exiting;
    for retry in 1..=result.attempts {
        let after = project(1, 0, 0, 0, 1, 0, 1, retry as i64, 0);
        validate("Contend", &before, &after);
        before = after;
    }
    assert!(!model.action_enabled("Flush", &before));
    assert!(!model.action_enabled("StallIo", &before));
    let joined = project(1, 0, 0, 0, 1, 0, 1, EXIT_LOCK_RETRIES as i64, 1);
    validate("Join", &before, &joined);

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(delta)
        .expect("the worker lane has room for one batch");
    drop(tx);
    drop(exit_tx);
    assert_eq!(
        run_kitty_writer(legacy.clone(), rx, exit_rx, None).sightings,
        1,
        "retry exhaustion must return the worker-owned batch to its joiner"
    );
    assert!(
        !legacy.exists(),
        "the genuine worker must not claim a contended batch was persisted"
    );
    drop(held);

    let dropped = project(1, 0, 0, 0, 0, 0, 1, 1, 0);
    assert!(
        !model.check_invariant("AcceptedConserved", &dropped),
        "negative control: dropping a contended batch must violate conservation"
    );
    let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
}

#[test]
fn flush_is_read_merge_write() {
    let p = tmp("merge");
    KittyLog::flush_merge(&p, &KittyLog::default());
    assert!(!p.exists(), "empty delta must not create the ledger");

    let lex = Lexicon::builtin();
    let mut theirs = KittyLog::default();
    theirs.record(&sighting(1), lex, "2026-07-01T09:00:00Z");
    theirs.entries[0].langs = vec!["en".to_string()];
    theirs.write(&p);
    let mut delta = KittyLog::default();
    delta.record(&sighting(2), lex, "2026-07-03T09:00:00Z");
    delta.entries[0].langs = vec!["ms".to_string()];
    let other = KittySighting {
        magic: KittyMagic::Sakura,
        ..sighting(3)
    };
    delta.record(&other, lex, "2026-07-02T09:00:00Z");
    KittyLog::flush_merge(&p, &delta);

    let merged = KittyLog::read(&p);
    assert_eq!(merged.sightings, 3, "counts sum");
    assert_eq!(merged.entries.len(), 2, "cells merge by key");
    let cell = &merged.entries[0];
    assert_eq!(cell.count, 2);
    assert_eq!(cell.first_seen, "2026-07-01T09:00:00Z", "min first_seen");
    assert_eq!(cell.last_seen, "2026-07-03T09:00:00Z", "max last_seen");
    assert_eq!(cell.langs, ["en", "ms"], "chips union");
    assert_eq!(merged.last_seen, "2026-07-03T09:00:00Z");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn first_flush_bootstraps_a_missing_config_directory_before_locking() {
    let seed = tmp("missing-parent");
    let root = seed.parent().unwrap().to_path_buf();
    std::fs::remove_dir_all(&root).unwrap();
    let path = root.join("fresh/profile").join(KITTY_LOG_FILE);
    let lex = Lexicon::builtin();
    let mut delta = KittyLog::default();
    delta.record(&sighting(1), lex, "2026-07-01T09:00:00Z");

    assert!(KittyLog::flush_merge(&path, &delta));
    assert_eq!(KittyLog::read_with_sidecar(&path).sightings, 1);
    assert!(path.with_extension("toml.lock").is_file());
    assert!(
        collectibles_path(&path)
            .with_extension("toml.lock")
            .is_file()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn transitional_collectibles_migrate_once_and_survive_a_rollback_rewrite() {
    let p = tmp("rollback-sidecar");
    let lex = Lexicon::builtin();
    let mut transitional = KittyLog::default();
    transitional.record(&sighting(1), lex, "2026-07-01T09:00:00Z");
    write_transitional_embedded(&p, &transitional);
    assert!(!collectibles_path(&p).exists());

    let migrated = KittyLog::read_with_sidecar(&p);
    assert_eq!(migrated.collectibles.len(), 1);
    assert_eq!(migrated.collectibles[0].count, 1);
    assert!(collectibles_path(&p).exists(), "load creates the sidecar");

    let mut rollback_view = KittyLog::read(&p);
    rollback_view.sightings = rollback_view.sightings.saturating_add(1);
    write_precollectibles_rewrite(&p, &rollback_view);
    assert!(KittyLog::read(&p).collectibles.is_empty());

    let after_rollback = KittyLog::read_with_sidecar(&p);
    assert_eq!(after_rollback.collectibles.len(), 1);
    assert_eq!(
        after_rollback.collectibles[0].count, 1,
        "the embedded migration must not be counted again"
    );
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn pre_baseline_sidecar_unions_rollback_key_with_replica_max_counts() {
    let p = tmp("pre-baseline-sidecar");
    let lex = Lexicon::builtin();
    let sample = sighting(88);
    let look_a = sample.look.normalized();
    let mut sidecar_a = KittyLog::default();
    assert!(sidecar_a.record_collectible(
        glyph_key(look_a.variant),
        look_a,
        &sample,
        lex,
        "2026-07-09T12:00:00Z",
    ));
    assert!(KittyLog::write_collectible_store_state(
        &collectibles_path(&p),
        &KittyCollectibleStore {
            collectibles: sidecar_a.collectibles.clone(),
            legacy_mirror: None,
        },
    ));

    let mut rollback = sidecar_a;
    let look_b = KittyLook {
        variant: CatGlyphId::S101,
        ..look_a
    };
    assert!(rollback.record_collectible(
        glyph_key(look_b.variant),
        look_b,
        &sample,
        lex,
        "2026-07-09T12:00:01Z",
    ));
    rollback.write(&p);

    let upgraded = KittyLog::read_with_sidecar(&p);
    assert_eq!(upgraded.collectibles.len(), 2, "A+B are unioned");
    assert_eq!(
        upgraded
            .collectibles
            .iter()
            .map(|row| row.count)
            .sum::<u64>(),
        2,
        "the physical A replica is not counted twice"
    );
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn replica_delta_distinguishes_reset_rows_from_stale_first_seen() {
    let baseline = KittyCollectible {
        count: 3,
        first_seen: "2026-07-01T00:00:00Z".into(),
        last_seen: "2026-07-03T00:00:00Z".into(),
        ..KittyCollectible::default()
    };
    let stale_legacy = KittyCollectible {
        count: 2,
        first_seen: "2026-07-02T00:00:00Z".into(),
        last_seen: "2026-07-02T00:00:00Z".into(),
        ..KittyCollectible::default()
    };
    assert_eq!(
        KittyLog::collectible_replica_delta(&stale_legacy, Some(&baseline)),
        0,
        "a stale later first_seen is not evidence of post-reset activity"
    );

    let recreated = KittyCollectible {
        count: 2,
        first_seen: "2026-07-04T00:00:00Z".into(),
        last_seen: "2026-07-05T00:00:00Z".into(),
        ..KittyCollectible::default()
    };
    assert_eq!(
        KittyLog::collectible_replica_delta(&recreated, Some(&baseline)),
        2,
        "a row born after the baseline window is entirely new"
    );
}

#[test]
fn sidecar_reconciles_rollback_discovery_and_repeat_without_double_counting() {
    let model = kitty_sidecar_durability_model();
    let project = |path: &Path, known: i64, events: i64| -> BTreeMap<_, _> {
        let count = |len: usize| i64::try_from(len).expect("bounded roster fits i64");
        let event_count = |rows: &[KittyCollectible]| {
            i64::try_from(rows.iter().map(|row| row.count).sum::<u64>())
                .expect("bounded test event count fits i64")
        };
        let base_rows = KittyLog::read(path).collectibles;
        let persisted = KittyLog::read_collectible_store(&collectibles_path(path));
        let sidecar_rows = persisted
            .as_ref()
            .map(|store| store.collectibles.as_slice())
            .unwrap_or_default();
        let baseline = persisted
            .as_ref()
            .and_then(|store| store.legacy_mirror.as_deref());
        let durable_rows = KittyLog::reconcile_collectible_replicas(
            persisted
                .as_ref()
                .map(|store| store.collectibles.as_slice()),
            baseline,
            &base_rows,
        );
        let pending = base_rows
            .iter()
            .filter(|row| !sidecar_rows.iter().any(|item| item.key == row.key))
            .count();
        let pending_events = base_rows
            .iter()
            .map(|row| {
                let baseline_row =
                    baseline.and_then(|rows| rows.iter().find(|item| item.key == row.key));
                KittyLog::collectible_replica_delta(row, baseline_row)
            })
            .sum::<u64>();
        [
            ("known", known),
            ("base", count(base_rows.len())),
            ("sidecar", count(sidecar_rows.len())),
            ("pending", count(pending)),
            (
                "pending_events",
                i64::try_from(pending_events).expect("bounded pending events fit i64"),
            ),
            ("durable", count(durable_rows.len())),
            ("events", events),
            ("base_events", event_count(&base_rows)),
            ("sidecar_events", event_count(sidecar_rows)),
            ("mirror_events", baseline.map_or(0, event_count)),
            ("durable_events", event_count(&durable_rows)),
        ]
        .into_iter()
        .collect()
    };

    let step = |prev: &mut BTreeMap<&'static str, i64>,
                spec: &mut BTreeMap<&'static str, i64>,
                after: BTreeMap<&'static str, i64>,
                action: &str,
                what: &str| {
        let (ok, why) =
            verify::validate_transition_tiered(&model, &[], prev, &after, Some(action), what);
        assert!(ok, "model rejected {what}: {why}");
        assert!(model.fire(action, spec), "{what}");
        assert_eq!(&after, spec, "{what}");
        *prev = after;
    };
    let rows = |path: &Path| KittyLog::read_with_sidecar(path).collectibles;
    let total = |rows: &[KittyCollectible]| rows.iter().map(|row| row.count).sum::<u64>();

    let path = tmp("sidecar-conformance");
    let mut prev = project(&path, 0, 0);
    let mut spec = model.init_state();
    assert_eq!(prev, spec);
    let sample = sighting(7000);
    let look = sample.look.normalized();
    let record = |log: &mut KittyLog, variant: CatGlyphId, at: &str| {
        log.record_collectible(
            glyph_key(variant),
            KittyLook { variant, ..look },
            &sample,
            Lexicon::builtin(),
            at,
        )
    };
    let mut discovery = KittyLog::default();
    assert!(record(&mut discovery, look.variant, "2026-07-09T12:00:00Z"));
    KittyLog::flush_merge(&path, &discovery);
    step(
        &mut prev,
        &mut spec,
        project(&path, 1, 1),
        "Discover",
        "discover A",
    );

    let mut rollback_view = KittyLog::read(&path);
    assert!(record(
        &mut rollback_view,
        CatGlyphId::S101,
        "2026-07-09T12:00:01Z"
    ));
    rollback_view.write(&path);
    step(
        &mut prev,
        &mut spec,
        project(&path, 2, 2),
        "OldDiscover",
        "rollback discovers B",
    );
    assert_eq!(rows(&path).len(), 2, "A+B survive re-upgrade");
    step(
        &mut prev,
        &mut spec,
        project(&path, 2, 2),
        "Reconcile",
        "re-upgrade unions B",
    );

    let mut rollback_repeat = KittyLog::read(&path);
    assert!(!record(
        &mut rollback_repeat,
        look.variant,
        "2026-07-09T12:00:02Z"
    ));
    rollback_repeat.write(&path);
    step(
        &mut prev,
        &mut spec,
        project(&path, 2, 3),
        "OldRepeat",
        "rollback repeats A",
    );
    let final_rows = rows(&path);
    assert_eq!(total(&final_rows), 3);
    assert_eq!(
        final_rows
            .iter()
            .find(|row| row.key == glyph_key(look.variant))
            .expect("A")
            .count,
        2,
        "mirrored A is not counted twice"
    );
    assert_eq!(
        total(&rows(&path)),
        3,
        "a second re-upgrade load is idempotent"
    );
    step(
        &mut prev,
        &mut spec,
        project(&path, 2, 3),
        "Reconcile",
        "re-upgrade imports one repeat",
    );

    write_precollectibles_rewrite(&path, &KittyLog::read(&path));
    step(
        &mut prev,
        &mut spec,
        project(&path, 2, 3),
        "OldRewrite",
        "pre-collectibles rewrite",
    );
    assert!(model.check_invariant("NoUnlockRollback", &prev));
    assert!(model.check_invariant("NoCountRollback", &prev));

    let mut empty_rollback = KittyLog::read(&path);
    assert!(empty_rollback.collectibles.is_empty());
    assert!(record(
        &mut empty_rollback,
        CatGlyphId::S102,
        "2026-07-09T12:00:03Z"
    ));
    empty_rollback.write(&path);
    step(
        &mut prev,
        &mut spec,
        project(&path, 3, 4),
        "OldDiscover",
        "rollback discovers C",
    );
    let reunioned = rows(&path);
    assert_eq!((reunioned.len(), total(&reunioned)), (3, 4), "A+B union C");
    step(
        &mut prev,
        &mut spec,
        project(&path, 3, 4),
        "Reconcile",
        "re-upgrade unions C",
    );
    write_precollectibles_rewrite(&path, &KittyLog::read(&path));
    step(
        &mut prev,
        &mut spec,
        project(&path, 3, 4),
        "OldRewrite",
        "second rewrite",
    );

    let mut reset_repeat = KittyLog::read(&path);
    assert!(reset_repeat.collectibles.is_empty());
    assert!(record(
        &mut reset_repeat,
        look.variant,
        "2026-07-09T12:00:04Z"
    ));
    reset_repeat.write(&path);
    step(
        &mut prev,
        &mut spec,
        project(&path, 3, 5),
        "OldRepeatReset",
        "repeat A after erasure",
    );
    let reset_rows = rows(&path);
    assert_eq!((reset_rows.len(), total(&reset_rows)), (3, 5));
    step(
        &mut prev,
        &mut spec,
        project(&path, 3, 5),
        "Reconcile",
        "re-upgrade imports the reset repeat",
    );
    write_precollectibles_rewrite(&path, &KittyLog::read(&path));
    step(
        &mut prev,
        &mut spec,
        project(&path, 3, 5),
        "OldRewrite",
        "final rewrite",
    );
    let restored = rows(&path);
    assert_eq!((restored.len(), total(&restored)), (3, 5));
    step(
        &mut prev,
        &mut spec,
        project(&path, 3, 5),
        "RestoreMirror",
        "re-upgrade restores the mirror",
    );

    let buggy_path = tmp("sidecar-negative");
    let mut embedded_only = KittyLog::default();
    assert!(record(
        &mut embedded_only,
        look.variant,
        "2026-07-09T12:00:00Z"
    ));
    write_transitional_embedded(&buggy_path, &embedded_only);
    assert!(KittyLog::write_collectible_store_state(
        &collectibles_path(&buggy_path),
        &KittyCollectibleStore {
            collectibles: Vec::new(),
            legacy_mirror: Some(embedded_only.collectibles.clone()),
        },
    ));
    let mut buggy_discovered = project(&buggy_path, 1, 1);
    buggy_discovered.insert("pending", 0);
    buggy_discovered.insert("durable", 1);
    buggy_discovered.insert("durable_events", 1);
    let (bug_admits, bug_why) = verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &model.init_state(),
        &buggy_discovered,
        Some("Discover"),
        "base-only collectible negative control",
    );
    assert!(
        bug_admits,
        "Buggy=1 must admit base-only storage: {bug_why}"
    );
    let (healthy_admits, healthy_why) = verify::validate_transition_tiered(
        &model,
        &[],
        &model.init_state(),
        &buggy_discovered,
        Some("Discover"),
        "base-only collectible healthy rejection",
    );
    assert!(
        !healthy_admits,
        "production model admitted base-only storage: {healthy_why}"
    );

    let old_view = KittyLog::read(&buggy_path);
    write_precollectibles_rewrite(&buggy_path, &old_view);
    let lost = project(&buggy_path, 1, 1);
    let (bug_admits, bug_why) = verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &buggy_discovered,
        &lost,
        Some("OldRewrite"),
        "base-only rollback loss",
    );
    assert!(
        bug_admits,
        "Buggy=1 must reproduce rollback loss: {bug_why}"
    );
    assert!(!model.check_invariant("NoUnlockRollback", &lost));
    assert!(!model.check_invariant("NoCountRollback", &lost));

    let _ = std::fs::remove_dir_all(path.parent().expect("scratch parent"));
    let _ = std::fs::remove_dir_all(buggy_path.parent().expect("scratch parent"));
}

#[test]
fn interleaved_flushes_preserve_first_discovery_look_and_chronology() {
    let p = tmp("collectible-order");
    let lex = Lexicon::builtin();
    let sample = sighting(90);
    let delta = |key: CatGlyphId, coat: u8, at: &str| {
        let mut log = KittyLog::default();
        let look = KittyLook {
            variant: key,
            coat,
            ..KittyLook::default()
        };
        assert!(log.record_collectible(glyph_key(key), look, &sample, lex, at));
        log
    };

    let late = delta(CatGlyphId::SpecSleeping, 12, "2026-07-02T00:00:00Z");
    let early = delta(CatGlyphId::SpecWitch, 3, "2026-07-01T00:00:00Z");
    let flush = |delta: &KittyLog| {
        let result = flush_pending_at_exit(&p, delta);
        assert!(
            result.persisted,
            "the semantic ordering check requires an accepted durable flush"
        );
    };
    flush(&late);
    flush(&early);

    let same_late = delta(CatGlyphId::SpecYarn, 14, "2026-07-03T00:00:00Z");
    let same_early = delta(CatGlyphId::SpecYarn, 1, "2026-06-30T00:00:00Z");
    flush(&same_late);
    flush(&same_early);

    let loaded = KittyLog::read_with_sidecar(&p);
    assert_eq!(
        loaded
            .collectibles
            .iter()
            .map(|item| item.key.as_str())
            .collect::<Vec<_>>(),
        ["spec_yarn", "spec_witch", "spec_sleeping"],
        "disk order follows first_seen, independent of flush order"
    );
    let yarn = &loaded.collectibles[0];
    assert_eq!(yarn.count, 2);
    assert_eq!(yarn.coat, 1, "earliest discovery owns the saved look");
    assert_eq!(yarn.first_seen, "2026-06-30T00:00:00Z");
    assert_eq!(yarn.last_seen, "2026-07-03T00:00:00Z");

    let mut host = KittyLogHost::load(Some(p.clone()));
    host.await_initial_load();
    assert_eq!(
        host.favourite_look(),
        None,
        "an unpinned ledger claims no companion on startup — the launch \
         kitty rides (the startup import no longer elects the latest \
         discovery; owner ruling, 2026-08-17)"
    );
    host.flush_exit();
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

fn look_sighting(ident: u64, look: KittyLook) -> KittySighting {
    KittySighting {
        look,
        traits: 0,
        ..sighting(ident)
    }
}

fn coated(variant: CatGlyphId, coat: u8) -> KittyLook {
    KittyLook {
        variant,
        coat,
        ..KittyLook::default()
    }
    .normalized()
}

#[test]
fn a_wear_reaches_the_delta_or_it_does_not_survive_a_restart() {
    let lex = Lexicon::builtin();
    let mut host = KittyLogHost::in_memory();
    let now = Instant::now();
    let old = coated(CatGlyphId::S100, 3);
    let new = coated(CatGlyphId::S101, 7);

    host.observe(4, [look_sighting(11, old)], lex, now, true);
    host.observe(4, [look_sighting(12, new)], lex, now, true);
    let keys: Vec<String> = host
        .log()
        .roster()
        .iter()
        .map(|row| row.key.clone())
        .collect();
    assert_eq!(keys.len(), 2, "fixture: two heads collected");
    host.take_delta_for_test();
    assert!(
        host.pending().roster().is_empty(),
        "fixture: the delta is drained, as it is after any flush"
    );

    let worn = host
        .wear(&keys[0], now, true)
        .expect("a collected cat can be worn");
    assert_eq!(host.favourite_look(), Some(worn), "it is on the cursor");

    let carried = host
        .pending()
        .roster()
        .iter()
        .find(|row| row.key == keys[0])
        .expect("the worn row reached the delta");
    assert!(
        !carried.favourite.is_empty(),
        "the delta row carries the pin stamp, which is the whole point"
    );
    assert_eq!(carried.count, 0, "a wear is not an observation");
}

fn pin_stamp(n: i64) -> String {
    format!("2026-09-25T00:00:{n:02}Z")
}

fn pin_tick(log: &KittyLog, key: &str) -> i64 {
    let row = log
        .roster()
        .iter()
        .find(|row| row.key == key)
        .expect("both cats are collected on both replicas");
    if row.favourite.is_empty() {
        return 0;
    }
    row.favourite
        .strip_prefix("2026-09-25T00:00:")
        .and_then(|rest| rest.strip_suffix('Z'))
        .and_then(|tick| tick.parse().ok())
        .unwrap_or_else(|| panic!("a stamp this test did not write: {}", row.favourite))
}

type PinFlush = fn(&mut KittyLog, &KittyLog);

fn join_flush(dst: &mut KittyLog, src: &KittyLog) {
    dst.merge_from(src);
}

fn take_flush(dst: &mut KittyLog, src: &KittyLog) {
    dst.merge_from(src);
    for row in &mut dst.collectibles {
        row.favourite = src
            .collectibles
            .iter()
            .find(|incoming| incoming.key == row.key)
            .map(|incoming| incoming.favourite.clone())
            .unwrap_or_default();
    }
}

fn pin_merge_walk(model: &Model, flush: PinFlush) -> Result<(usize, usize), String> {
    let lex = Lexicon::builtin();
    let mut replica = KittyLog::default();
    for (ident, look) in [
        (11, coated(CatGlyphId::S100, 3)),
        (12, coated(CatGlyphId::S101, 7)),
    ] {
        replica.record(&look_sighting(ident, look), lex, "2026-09-24T00:00:00Z");
    }
    let keys: Vec<String> = replica.roster().iter().map(|row| row.key.clone()).collect();
    assert_eq!(keys.len(), 2, "fixture: two cats collected");
    let (one, two) = (keys[0].as_str(), keys[1].as_str());
    let project = |a: &KittyLog, b: &KittyLog, book: &aterm_spec::interp::State| {
        let mut state = book.clone();
        state.insert("a1", pin_tick(a, one));
        state.insert("a2", pin_tick(a, two));
        state.insert("b1", pin_tick(b, one));
        state.insert("b2", pin_tick(b, two));
        state
    };

    let init = model.init_state();
    if project(&replica, &replica, &init) != init {
        return Err(format!("the fixture does not project to {init:?}"));
    }
    let mut seen = std::collections::BTreeSet::from([init.clone()]);
    let mut queue = std::collections::VecDeque::from([(init, replica.clone(), replica)]);
    let (mut states, mut edges) = (0usize, 0usize);
    while let Some((state, a, b)) = queue.pop_front() {
        states += 1;
        for action in ["WearOneOnA", "WearTwoOnB", "FlushAToB", "FlushBToA"] {
            let expected = model.successors(action, &state);
            if expected.is_empty() {
                continue;
            }
            let (mut a, mut b, mut book) = (a.clone(), b.clone(), state.clone());
            *book.get_mut("steps").expect("steps") += 1;
            let hi = |book: &mut aterm_spec::interp::State, hi: &'static str, pre: &str| {
                let now = book[hi].max(state[pre]);
                book.insert(hi, now);
            };
            match action {
                "WearOneOnA" | "WearTwoOnB" => {
                    let stamp = pin_stamp(state["clock"] + 1);
                    let worn = if action == "WearOneOnA" {
                        a.wear(one, &stamp)
                    } else {
                        hi(&mut book, "b2_hi", "b2");
                        b.wear(two, &stamp)
                    };
                    worn.expect("a collected cat can be worn");
                    *book.get_mut("clock").expect("clock") += 1;
                    book.insert("ab", 0);
                }
                "FlushAToB" => {
                    hi(&mut book, "b1_hi", "b1");
                    hi(&mut book, "b2_hi", "b2");
                    flush(&mut b, &a);
                    book.insert("ab", 1);
                }
                _ => flush(&mut a, &b),
            }
            let after = project(&a, &b, &book);
            if expected != vec![after.clone()] {
                return Err(format!(
                    "{}: real {action} took {state:?} to {after:?}, the model's {action} \
                     to {expected:?}",
                    model.name
                ));
            }
            edges += 1;
            if seen.insert(after.clone()) {
                queue.push_back((after, a, b));
            }
        }
    }
    Ok((states, edges))
}

#[test]
fn wear_and_merge_conform_to_the_kitty_pin_merge_model() {
    let model = kitty_pin_merge_model();
    let (states, edges) = pin_merge_walk(&model, join_flush).unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(
        Ok(states),
        aterm_spec::interp::bmc(&model),
        "the real replicas must reach every state the model can"
    );
    assert!(edges > states, "every state is left more than one way");

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let rejected = pin_merge_walk(&model, take_flush)
        .expect_err("the healthy model must reject an assignment merge");
    assert!(
        rejected.contains("the model's FlushAToB") || rejected.contains("the model's FlushBToA"),
        "the assignment merge must be rejected at a flush: {rejected}"
    );
    pin_merge_walk(&buggy, take_flush)
        .unwrap_or_else(|why| panic!("the buggy model must describe the take: {why}"));
    pin_merge_walk(&buggy, join_flush)
        .expect_err("the shipped join must not conform to the assignment merge");
}

#[test]
fn favourite_pins_an_already_collected_head() {
    let lex = Lexicon::builtin();
    let mut host = KittyLogHost::in_memory();
    let stumbled = coated(CatGlyphId::S100, 3);
    let pinned = coated(CatGlyphId::S100, 9);
    let now = Instant::now();

    host.observe(4, [look_sighting(11, stumbled)], lex, now, true);
    assert_eq!(
        host.favourite_look(),
        None,
        "a stumbled discovery collects but claims nothing"
    );

    host.favourite(&look_sighting(12, pinned), lex, now, true);

    assert_eq!(
        host.favourite_look(),
        Some(pinned),
        "the pin wins even though this glyph was already collected"
    );
    let rows: Vec<_> = host
        .log()
        .collectibles
        .iter()
        .filter(|item| item.key == glyph_key(CatGlyphId::S100))
        .collect();
    assert_eq!(rows.len(), 1, "a favourite re-stamps, it never duplicates");
    assert_eq!(rows[0].coat, 9, "the pin takes composition ownership");
    assert!(!rows[0].favourite.is_empty(), "and is stamped durably");
}

#[test]
fn a_later_discovery_does_not_steal_a_pinned_favourite() {
    let lex = Lexicon::builtin();
    let mut host = KittyLogHost::in_memory();
    let pinned = coated(CatGlyphId::S100, 5);
    let newcomer = coated(CatGlyphId::SpecWitch, 12);
    let now = Instant::now();

    host.favourite(&look_sighting(21, pinned), lex, now, true);
    let discovery = host.observe(4, [look_sighting(22, newcomer)], lex, now, true);

    assert_eq!(
        discovery,
        Some(newcomer),
        "a genuine unlock still reports itself, so its hello still plays"
    );
    assert_eq!(
        host.favourite_look(),
        Some(pinned),
        "…but the pin is untouched"
    );
}

#[test]
fn an_ambient_discovery_collects_without_touching_the_companion() {
    let lex = Lexicon::builtin();
    let mut host = KittyLogHost::in_memory();
    let newcomer = coated(CatGlyphId::SpecWitch, 12);
    let now = Instant::now();

    host.observe_ambient(4, [look_sighting(41, newcomer)], lex, now, true);

    assert_eq!(
        host.favourite_look(),
        None,
        "watching output earns no companion"
    );
    assert!(
        host.log()
            .collectibles
            .iter()
            .any(|item| item.key == glyph_key(CatGlyphId::SpecWitch)),
        "…but the collectible row still mints"
    );
    assert_eq!(host.log().sightings, 1, "and the ledger still counts");

    let pinned = coated(CatGlyphId::S100, 3);
    host.favourite(&look_sighting(51, pinned), lex, now, true);
    let printed = coated(CatGlyphId::S101, 12);
    host.observe_ambient(4, [look_sighting(52, printed)], lex, now, true);
    assert_eq!(
        host.favourite_look(),
        Some(pinned),
        "output text never re-dresses the companion"
    );
}

#[test]
fn the_favourite_wins_the_merge_against_an_earlier_discovery() {
    let key = glyph_key(CatGlyphId::S100).to_string();
    let stumbled = KittyCollectible {
        key: key.clone(),
        variant: key,
        coat: 3,
        age: age_key(KittyLook::default().age).to_string(),
        count: 1,
        first_seen: "2026-07-01T00:00:00Z".to_string(),
        last_seen: "2026-07-01T00:00:00Z".to_string(),
        ..KittyCollectible::default()
    };
    let pinned = KittyCollectible {
        coat: 9,
        first_seen: "2026-07-05T00:00:00Z".to_string(),
        last_seen: "2026-07-05T00:00:00Z".to_string(),
        favourite: "2026-07-05T00:00:00Z".to_string(),
        ..stumbled.clone()
    };
    let merged = |first: &KittyCollectible, second: &KittyCollectible| {
        let mut log = KittyLog {
            collectibles: vec![first.clone()],
            ..KittyLog::default()
        };
        log.merge_from(&KittyLog {
            collectibles: vec![second.clone()],
            ..KittyLog::default()
        });
        log.collectibles
    };

    for rows in [merged(&stumbled, &pinned), merged(&pinned, &stumbled)] {
        assert_eq!(rows.len(), 1, "one semantic key, one row");
        assert_eq!(
            rows[0].coat, 9,
            "the favourite owns the look, not the earliest discovery"
        );
        assert_eq!(rows[0].favourite, "2026-07-05T00:00:00Z");
        assert_eq!(
            rows[0].first_seen, "2026-07-01T00:00:00Z",
            "the pin never rewrites when the cat was first met"
        );
    }
}

#[test]
fn a_favourite_survives_the_startup_import() {
    let lex = Lexicon::builtin();
    let p = tmp("favourite-restart");
    let pinned = coated(CatGlyphId::S100, 7);
    let later = coated(CatGlyphId::S101, 2);
    assert!(glyph_key(CatGlyphId::S100) < glyph_key(CatGlyphId::S101));

    let mut host = KittyLogHost::load_with_writer_spawn(Some(p.clone()), |_| None);
    let now = Instant::now();
    host.favourite(&look_sighting(31, pinned), lex, now, true);
    host.observe(4, [look_sighting(32, later)], lex, now, true);
    assert!(
        KittyLog::flush_merge(&p, &host.delta),
        "the synchronous flush must land for the restart to mean anything"
    );

    let mut restarted = KittyLogHost::load(Some(p.clone()));
    restarted.await_initial_load();
    assert_eq!(
        restarted.favourite_look(),
        Some(pinned),
        "the pin outranks the chronologically latest discovery on restart"
    );
    restarted.flush_exit();
    let _ = std::fs::remove_dir_all(p.parent().expect("scratch parent"));
}

#[test]
fn a_favourite_is_never_absorbed_by_the_recount_ring() {
    let lex = Lexicon::builtin();
    let mut host = KittyLogHost::in_memory();
    let look = coated(CatGlyphId::S100, 4);
    let now = Instant::now();
    let before = host.revision();

    host.favourite(&look_sighting(77, look), lex, now, true);
    host.favourite(&look_sighting(77, look), lex, now, true);

    assert_eq!(
        host.log().sightings,
        2,
        "the cat appeared again because the user asked again"
    );
    assert_eq!(
        host.revision(),
        before.wrapping_add(2),
        "each press is its own change stamp"
    );
}

#[test]
fn favourite_with_recording_off_is_memory_only() {
    let lex = Lexicon::builtin();
    let mut host = KittyLogHost::in_memory();
    let look = coated(CatGlyphId::S100, 6);

    host.favourite(&look_sighting(88, look), lex, Instant::now(), false);

    assert_eq!(
        host.favourite_look(),
        Some(look),
        "the pick still holds for this session"
    );
    assert_eq!(host.log().sightings, 0, "the ledger tier stayed closed");
    assert!(
        host.log().collectibles.is_empty(),
        "and no roster row was minted"
    );
    assert!(
        host.is_favourite(look),
        "the palette checkmark reads the same pin the verdict wears — even \
         a memory-only pin the roster never sees"
    );
}

#[test]
fn a_session_pin_is_not_clobbered_by_a_late_startup_import() {
    let lex = Lexicon::builtin();
    let now = Instant::now();
    let session_pick = coated(CatGlyphId::S100, 6);
    let disk_pin = coated(CatGlyphId::S101, 2);
    let mut on_disk = KittyLog::default();
    let _ = on_disk.record(&look_sighting(1, disk_pin), lex, "2026-08-01T00:00:00Z");
    on_disk.favourite_collectible(disk_pin, "2026-08-01T00:00:00Z");
    assert_eq!(
        on_disk.favourite_look(),
        Some(disk_pin),
        "fixture: the disk carries a pin"
    );

    let mut untouched = KittyLogHost::in_memory();
    untouched.absorb_initial(on_disk.clone());
    assert_eq!(untouched.favourite_look(), Some(disk_pin));

    let mut host = KittyLogHost::in_memory();
    host.favourite(&look_sighting(88, session_pick), lex, now, false);
    host.absorb_initial(on_disk);
    assert_eq!(
        host.favourite_look(),
        Some(session_pick),
        "the session's explicit pick outranks the imported pin"
    );
    assert!(host.is_favourite(session_pick), "and the checkmark agrees");
}

#[test]
fn observe_never_spawns_after_writer_prearm_failure() {
    let lex = Lexicon::builtin();
    let ledger = tmp("writer-prearm-failure");
    let spawn_calls = std::cell::Cell::new(0usize);
    let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), |path| {
        spawn_calls.set(spawn_calls.get() + 1);
        assert_eq!(path, ledger);
        None
    });
    assert_eq!(
        spawn_calls.get(),
        1,
        "construction makes one pre-arm attempt"
    );
    assert!(host.writer.is_none(), "the injected pre-arm failed");

    host.observe(7, [sighting(11)], lex, Instant::now(), true);
    assert_eq!(
        spawn_calls.get(),
        1,
        "first-sighting observe must be try-send-only, never spawn"
    );
    assert!(
        host.writer.is_none(),
        "observe must not bypass the construction spawner and create a worker"
    );

    host.flush_exit();
    assert!(
        !ledger.exists(),
        "a failed pre-arm degrades observability instead of doing exit IO"
    );
    let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
}

#[test]
fn disconnected_prearmed_writer_keeps_delta_in_memory_without_exit_io() {
    let lex = Lexicon::builtin();
    let ledger = tmp("writer-disconnected");
    let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), |_| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel(1);
        drop(rx);
        drop(exit_rx);
        let handle = std::thread::spawn(KittyLog::default);
        Some(KittyWriter {
            tx,
            exit_tx,
            initial: None,
            handle,
        })
    });

    host.observe(7, [sighting(11)], lex, Instant::now(), true);
    assert!(
        host.writer.is_none(),
        "a disconnected render-side sender is detached, never joined"
    );
    assert_eq!(host.delta.sightings, 1, "the rejected batch stays owned");
    host.flush_exit();
    assert!(
        !ledger.exists(),
        "a failed pre-arm must not fall back to synchronous exit IO"
    );
    let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
}

#[test]
fn exit_detaches_a_filesystem_stalled_worker_at_the_deadline() {
    let lex = Lexicon::builtin();
    let ledger = tmp("writer-stalled-exit");
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), |_| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            let owned_batch = rx.recv().unwrap_or_default();
            let _ = release_rx.recv();
            drop(exit_rx);
            owned_batch
        });
        Some(KittyWriter {
            tx,
            exit_tx,
            initial: None,
            handle,
        })
    });
    host.observe(7, [sighting(11)], lex, Instant::now(), true);

    host.flush_exit();

    let model = kitty_flush_worker_model();
    let project = |stalled, deadline, detached| {
        [
            ("accepted", 1),
            ("normal_lane", 0),
            ("host_tail", 0),
            ("exit_lane", 0),
            ("pending", 1),
            ("persisted", 0),
            ("exiting", 1),
            ("retries", 0),
            ("joined", 0),
            ("stalled", stalled),
            ("deadline", deadline),
            ("detached", detached),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
    };
    let before_stall = project(0, 0, 0);
    let stalled = project(1, 0, 0);
    let tick_one = project(1, 1, 0);
    let tick_two = project(1, 2, 0);
    let detached = project(1, 2, 1);
    for (action, before, after) in [
        ("StallIo", &before_stall, &stalled),
        ("TickDeadline", &stalled, &tick_one),
        ("TickDeadline", &tick_one, &tick_two),
        ("Detach", &tick_two, &detached),
    ] {
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            "real Kitty Log stalled-IO exit deadline",
        );
        assert!(ok, "model rejected {action}: {why}");
    }
    let early_detach = project(1, 0, 1);
    assert!(
        !model.check_invariant("DetachedOnlyAtDeadline", &early_detach),
        "negative control: detaching before the deadline must be rejected"
    );
    let _ = release_tx.send(());
    let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
}

#[test]
fn contended_batch_retries_after_lock_release_without_a_new_sighting() {
    let lex = Lexicon::builtin();
    let ledger = tmp("writer-lock-retry");
    let sidecar = collectibles_path(&ledger);
    let locks = lock_pair(&ledger, &sidecar).expect("hold both ledger locks");
    let mut host = KittyLogHost::load(Some(ledger.clone()));
    host.observe(7, [sighting(11)], lex, Instant::now(), true);
    drop(locks);
    let deadline = Instant::now() + FLUSH_RETRY_DELAY + aterm_spec::wait::MUST_HAPPEN;
    while !ledger.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ledger.exists(),
        "the pending worker batch retries without another sighting"
    );
    host.flush_exit();
    assert_eq!(KittyLog::read_with_sidecar(&ledger).sightings, 1);
    let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
}

fn read_once_persisted(ledger: &Path, sightings: u64) -> KittyLog {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while KittyLog::read(ledger).sightings < sightings && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    KittyLog::read_with_sidecar(ledger)
}

#[test]
fn full_normal_queue_cannot_drop_the_dedicated_exit_tail() {
    let ledger = tmp("writer-full-normal-exit-tail");
    let lex = Lexicon::builtin();
    let (pending_tx, pending_rx) = std::sync::mpsc::channel();
    let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), move |path| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
        let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
        let handle = std::thread::spawn(move || {
            let Ok(mut pending) = rx.recv() else {
                return KittyLog::default();
            };
            let _ = pending_tx.send(());
            let Ok(tail) = exit_rx.recv() else {
                return pending;
            };
            let Ok(normal) = rx.recv() else {
                pending.merge_from(&tail);
                return pending;
            };
            pending.merge_from(&normal);
            pending.merge_from(&tail);
            if KittyLog::flush_merge(&path, &pending) {
                KittyLog::default()
            } else {
                pending
            }
        });
        Some(KittyWriter {
            tx,
            exit_tx,
            initial: None,
            handle,
        })
    });
    let t0 = Instant::now();
    host.observe(7, [sighting(11)], lex, t0, true);
    assert!(host.delta.is_empty(), "the normal queue accepted batch one");
    pending_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the worker owns batch one before the queue is refilled");
    host.observe(8, [sighting(22)], lex, t0 + FLUSH_DEBOUNCE, true);
    assert!(host.delta.is_empty(), "the normal queue accepted batch two");
    host.observe(
        9,
        [sighting(33)],
        lex,
        t0 + FLUSH_DEBOUNCE + Duration::from_secs(1),
        true,
    );
    assert_eq!(
        host.delta.sightings, 1,
        "batch three remains host-owned while pending and normal are occupied"
    );

    let model = kitty_flush_worker_model();
    let project =
        |accepted, normal_lane, host_tail, exit_lane, pending, persisted, exiting, joined| {
            [
                ("accepted", accepted),
                ("normal_lane", normal_lane),
                ("host_tail", host_tail),
                ("exit_lane", exit_lane),
                ("pending", pending),
                ("persisted", persisted),
                ("exiting", exiting),
                ("retries", 0),
                ("joined", joined),
                ("stalled", 0),
                ("deadline", 0),
                ("detached", 0),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
        };
    let validate = |action: &str,
                    before: &BTreeMap<&'static str, i64>,
                    after: &BTreeMap<&'static str, i64>| {
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            "Kitty Log dedicated exit-tail delivery",
        );
        assert!(ok, "model rejected {action}: {why}");
    };
    let initial = model.init_state();
    let queued_one = project(1, 1, 0, 0, 0, 0, 0, 0);
    validate("QueueNormal", &initial, &queued_one);
    let pending_one = project(1, 0, 0, 0, 1, 0, 0, 0);
    validate("DrainNormal", &queued_one, &pending_one);
    let queued_two = project(2, 1, 0, 0, 1, 0, 0, 0);
    validate("QueueNormal", &pending_one, &queued_two);
    let retained = project(3, 1, 1, 0, 1, 0, 0, 0);
    validate("RetainTailOnFull", &queued_two, &retained);
    let exiting = project(3, 1, 1, 0, 1, 0, 1, 0);
    validate("BeginExit", &retained, &exiting);
    let offered = project(3, 1, 0, 1, 1, 0, 1, 0);
    validate("OfferTail", &exiting, &offered);
    let normal_pending = project(3, 0, 0, 1, 2, 0, 1, 0);
    validate("DrainNormal", &offered, &normal_pending);
    let both_pending = project(3, 0, 0, 0, 3, 0, 1, 0);
    validate("AbsorbTail", &normal_pending, &both_pending);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut dropped_tail = buggy.init_state();
    for action in [
        "QueueNormal",
        "DrainNormal",
        "QueueNormal",
        "RetainTailOnFull",
        "BeginExit",
        "OfferTail",
    ] {
        assert!(buggy.fire(action, &mut dropped_tail), "buggy {action}");
    }
    assert_eq!(dropped_tail["accepted"], 3);
    assert_eq!(dropped_tail["pending"], 1);
    assert_eq!(dropped_tail["normal_lane"], 1);
    assert!(
        !buggy.check_invariant("AcceptedConserved", &dropped_tail),
        "negative control: the retired saturated one-lane exit send must lose ownership"
    );

    host.flush_exit();
    assert!(
        host.delta.is_empty(),
        "successful shutdown transfers and persists every owned batch"
    );
    let saved = read_once_persisted(&ledger, 3);
    assert_eq!(saved.sightings, 3);
    assert_eq!(saved.collectibles.len(), 1);
    assert_eq!(saved.collectibles[0].count, 3);
    let persisted = project(3, 0, 0, 0, 0, 3, 1, 0);
    validate("Flush", &both_pending, &persisted);
    let joined = project(3, 0, 0, 0, 0, 3, 1, 1);
    validate("Join", &persisted, &joined);
    let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
}

#[test]
fn background_writer_persists_and_drains_on_exit() {
    let lex = Lexicon::builtin();
    let ledger = tmp("writer");
    let mut host = KittyLogHost::load(Some(ledger.clone()));
    let writer_id = host
        .writer
        .as_ref()
        .expect("persistent host pre-arms its writer")
        .handle
        .thread()
        .id();
    let t0 = Instant::now();
    host.observe(7, [sighting(11)], lex, t0, true);
    assert_eq!(
        host.writer
            .as_ref()
            .map(|writer| writer.handle.thread().id()),
        Some(writer_id),
        "observe reuses the pre-armed worker"
    );
    host.observe(8, [sighting(22)], lex, t0 + Duration::from_secs(1), true);
    host.flush_exit();
    let saved = read_once_persisted(&ledger, 2);
    assert_eq!(
        saved.sightings, 2,
        "both sightings persisted across the background writer + exit drain"
    );
    assert_eq!(saved.collectibles.len(), 1);
    assert_eq!(saved.collectibles[0].count, 2);
    let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
}

#[test]
fn dedupe_ring_absorbs_recounts() {
    let lex = Lexicon::builtin();
    let mut host = KittyLogHost::in_memory();
    let t0 = Instant::now();
    host.observe(7, [sighting(42)], lex, t0, true);
    assert_eq!(host.log().sightings, 1);
    let rev = host.revision();
    host.observe(7, [sighting(42)], lex, t0, true);
    host.observe(7, [sighting(42)], lex, t0 + Duration::from_secs(60), true);
    assert_eq!(host.log().sightings, 1, "ring absorbs the recounts");
    assert_eq!(host.revision(), rev, "no revision bump when suppressed");
    host.observe(8, [sighting(42)], lex, t0, true);
    assert_eq!(host.log().sightings, 2);
    host.observe(
        7,
        [sighting(42)],
        lex,
        t0 + Duration::from_secs(60) + RING_TTL + Duration::from_secs(1),
        true,
    );
    assert_eq!(host.log().sightings, 3, "an expired episode recounts");
    let rev = host.revision();
    host.observe(9, [sighting(1)], lex, t0, false);
    assert_eq!(host.log().sightings, 3, "log=false drains-and-drops");
    assert_eq!(host.revision(), rev);
}

#[test]
fn generated_collectible_set_conforms_and_rejects_duplicate_growth() {
    let model = kitty_collectibles_model();
    let roster_cap = model
        .consts
        .iter()
        .find_map(|(name, value)| (*name == "RosterCap").then_some(*value))
        .expect("model declares RosterCap");
    assert_eq!(
        usize::try_from(roster_cap).expect("non-negative roster cap"),
        GLYPH_IDS.len(),
        "generated art and its verified capacity must change together"
    );

    let lex = Lexicon::builtin();
    let sample = sighting(700);
    let look = sample.look.normalized();
    let mut log = KittyLog::default();
    let mut calls = 0i64;
    let mut discoveries = 0i64;
    let mut spec = model.init_state();
    let project = |log: &KittyLog, calls: i64, discoveries: i64| -> BTreeMap<_, _> {
        [
            (
                "unlocked",
                i64::try_from(log.collectibles.len()).expect("bounded roster fits i64"),
            ),
            ("discoveries", discoveries),
            ("sightings", calls),
            ("duplicates", calls - discoveries),
        ]
        .into_iter()
        .collect()
    };

    for &glyph in GLYPH_IDS {
        let prev = project(&log, calls, discoveries);
        assert_eq!(prev, spec);
        let is_new =
            log.record_collectible(glyph_key(glyph), look, &sample, lex, "2026-07-09T12:00:00Z");
        assert!(is_new, "first semantic-key insertion must discover");
        calls += 1;
        discoveries += 1;
        let next = project(&log, calls, discoveries);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &prev,
            &next,
            Some("Unlock"),
            "real generated collectible unlock",
        );
        assert!(ok, "model rejected real unique insertion: {why}");
        assert!(model.fire("Unlock", &mut spec));
        assert_eq!(next, spec);
    }
    assert_eq!(log.collectibles.len(), GLYPH_IDS.len());

    let prev = project(&log, calls, discoveries);
    let first_key = glyph_key(GLYPH_IDS[0]);
    let first_count = log.collectibles[0].count;
    let repeat_discovered =
        log.record_collectible(first_key, look, &sample, lex, "2026-07-09T12:00:01Z");
    assert!(!repeat_discovered);
    assert_eq!(log.collectibles[0].count, first_count + 1);
    calls += 1;
    let next = project(&log, calls, discoveries);
    let (ok, why) = verify::validate_transition_tiered(
        &model,
        &[],
        &prev,
        &next,
        Some("Repeat"),
        "real generated collectible repeat",
    );
    assert!(ok, "model rejected real duplicate insertion: {why}");
    assert!(model.fire("Repeat", &mut spec));
    assert_eq!(next, spec);

    let unchanged = log.clone();
    assert!(!log.record_collectible(
        "not_a_generated_cat",
        look,
        &sample,
        lex,
        "2026-07-09T12:00:02Z",
    ));
    assert_eq!(log, unchanged, "unknown keys cannot grow the set");

    let mut duplicate_prev = model.init_state();
    assert!(model.fire("Unlock", &mut duplicate_prev));
    let mut duplicate_growth = duplicate_prev.clone();
    duplicate_growth.insert("unlocked", 2);
    duplicate_growth.insert("discoveries", 2);
    duplicate_growth.insert("sightings", 2);
    duplicate_growth.insert("duplicates", 1);
    let (bug_admits, bug_why) = verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &duplicate_prev,
        &duplicate_growth,
        Some("Repeat"),
        "collectible duplicate-growth negative control",
    );
    assert!(
        bug_admits,
        "Buggy=1 must reproduce duplicate growth: {bug_why}"
    );
    let (healthy_admits, healthy_why) = verify::validate_transition_tiered(
        &model,
        &[],
        &duplicate_prev,
        &duplicate_growth,
        Some("Repeat"),
        "collectible duplicate-growth healthy rejection",
    );
    assert!(
        !healthy_admits,
        "production model accepted duplicate growth: {healthy_why}"
    );

    let path = tmp("collectible-normalize");
    let mut edited = log.clone();
    edited.collectibles.extend(log.collectibles.clone());
    edited.collectibles.push(KittyCollectible {
        key: "future_unknown".to_string(),
        variant: "future_unknown".to_string(),
        ..KittyCollectible::default()
    });
    assert!(KittyLog::write_collectible_store(
        &collectibles_path(&path),
        &edited.collectibles,
    ));
    let loaded = KittyLog::read_with_sidecar(&path);
    assert_eq!(loaded.collectibles.len(), GLYPH_IDS.len());
    let mut expected = GLYPH_IDS
        .iter()
        .map(|&glyph| glyph_key(glyph))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(
        loaded
            .collectibles
            .iter()
            .map(|item| item.key.as_str())
            .collect::<Vec<_>>(),
        expected,
        "equal timestamps use semantic-key order"
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("scratch parent"));
}

#[test]
fn persisted_collectibles_repair_accessory_bases_from_their_keys() {
    let path = tmp("collectible-look-repair");
    let edited = KittyLog {
        collectibles: vec![
            KittyCollectible {
                key: "acc_bow".to_string(),
                variant: "acc_bow".to_string(),
                accessory: "acc_crown".to_string(),
                coat: u8::MAX,
                iris: u8::MAX,
                age: "future-age".to_string(),
                count: 1,
                ..KittyCollectible::default()
            },
            KittyCollectible {
                key: "s1_00".to_string(),
                variant: "acc_crown".to_string(),
                accessory: "acc_bow".to_string(),
                count: 1,
                ..KittyCollectible::default()
            },
            KittyCollectible {
                key: "spec_sleeping".to_string(),
                variant: "acc_bow".to_string(),
                accessory: "acc_crown".to_string(),
                count: 1,
                ..KittyCollectible::default()
            },
        ],
        ..KittyLog::default()
    };
    assert!(KittyLog::write_collectible_store(
        &collectibles_path(&path),
        &edited.collectibles,
    ));

    let loaded = KittyLog::read_with_sidecar(&path);
    assert_eq!(loaded.collectibles.len(), 3);

    let bow = &loaded.collectibles[0];
    assert_eq!(bow.variant, glyph_key(KittyLook::default().variant));
    assert_eq!(bow.accessory, "acc_bow");
    assert_eq!((bow.coat, bow.iris), (15, 7));
    assert_eq!(bow.age, "adult");
    let bow_look = bow.look().expect("canonical bow look");
    assert_eq!(GLYPHS[bow_look.variant as usize].kind, GlyphKind::Head);
    assert_eq!(bow_look.accessory, Some(CatGlyphId::AccBow));

    let head = &loaded.collectibles[1];
    assert_eq!(head.variant, "s1_00");
    assert_eq!(head.accessory, "");
    assert_eq!(
        head.look().expect("canonical head look").variant,
        CatGlyphId::S100
    );

    let special = &loaded.collectibles[2];
    assert_eq!(special.variant, "spec_sleeping");
    assert_eq!(special.accessory, "");
    assert_eq!(
        special.look().expect("canonical special look").variant,
        CatGlyphId::SpecSleeping
    );

    let _ = std::fs::remove_dir_all(path.parent().expect("scratch parent"));
}
