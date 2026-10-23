// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use atpkg::vendor_direct::watch::{ConcurrentVendorGetFn, HeadWatch};
use atpkg::{VendorFetchError, VendorGet};
#[cfg(unix)]
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::SystemTime;

const CLAUDE_HEAD: &str = "https://downloads.claude.ai/claude-code-releases/latest";

fn modeled_park_choice(end: &ParkEnd) -> i64 {
    match end {
        ParkEnd::Bumped => 1,
        ParkEnd::Elapsed => 2,
        ParkEnd::StoreUnlocked => 5,
        ParkEnd::IndexPublished(_) => 3,
        ParkEnd::VendorMoved(_) => 4,
        ParkEnd::SwitchedOff => 0,
        ParkEnd::FlipDue => 1,
    }
}

fn modeled_index_owner(watch: &BumpWatch) -> i64 {
    i64::from(watch.pending_index_probe.is_some())
}

/// A store whose `claude` is an old index build, older than any head.
fn store_with_legacy_claude(label: &str) -> atpkg::store::Layout {
    let prefix =
        std::env::temp_dir().join(format!("aterm-head-watch-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&prefix);
    let layout = atpkg::store::Layout { prefix };
    let build = layout.build_dir("claude", 2_026_092_201);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    atpkg::atomic_symlink(&build, &layout.program_current("claude")).unwrap();
    layout
}

/// The claude head at `version`.
fn vendor(
    version: &'static str,
    asked: Arc<Mutex<Vec<Option<String>>>>,
) -> impl Fn(&str, &str, u64, Option<&str>) -> Result<VendorGet, VendorFetchError> + Send + Sync {
    move |program, url, _cap, etag| {
        assert_eq!((program, url), ("claude", CLAUDE_HEAD));
        asked.lock().unwrap().push(etag.map(str::to_string));
        let tag = format!("\"{version}\"");
        if etag == Some(tag.as_str()) {
            return Ok(VendorGet::NotModified { etag: tag });
        }
        Ok(VendorGet::Body {
            bytes: version.as_bytes().to_vec(),
            etag: Some(tag),
            effective_url: url.to_string(),
        })
    }
}

#[test]
fn a_targeted_pass_names_its_program_on_the_lanes_own_argv() {
    let layout = atpkg::store::Layout {
        prefix: std::path::PathBuf::from("/p"),
    };
    let progress = layout.progress_file().into_os_string();
    let words = |verb, layout| {
        lane_pass_args(verb, layout, None)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let progress = progress.to_string_lossy().into_owned();
    assert_eq!(
        words(PassVerb::UpdateProgram("claude"), Some(&layout)),
        [
            "update",
            "claude",
            "--head-watch",
            "--wait-lock",
            "1800",
            "--progress-file",
            &progress
        ]
    );
    assert_eq!(
        words(PassVerb::UpdateProgram("codex"), None),
        ["update", "codex", "--head-watch", "--wait-lock", "1800"],
        "the door is the flag, never the progress file"
    );
    assert_eq!(
        words(PassVerb::Update, Some(&layout)),
        [
            "update",
            atpkg::cli::DEFER_BUSY_FLIP_FLAG,
            "--wait-lock",
            "1800",
            "--progress-file",
            &progress
        ],
        "the loop's whole pass holds a busy toolchain's flip for quiet"
    );
    assert_eq!(
        lane_pass_args(PassVerb::Update, Some(&layout), Some(45))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [
            "update",
            atpkg::cli::PUBLISHED_INDEX_HINT_FLAG,
            "45",
            atpkg::cli::DEFER_BUSY_FLIP_FLAG,
            "--wait-lock",
            "1800",
            "--progress-file",
            &progress,
        ],
        "a published wake carries its target into the lock waiter and pass-end record"
    );
    assert_eq!(words(PassVerb::Seed, None), ["seed", "--wait-lock", "1800"]);
    for verb in [
        PassVerb::Update,
        PassVerb::Seed,
        PassVerb::UpdateProgram("claude"),
    ] {
        for hint in [None, Some(45)] {
            for l in [None, Some(&layout)] {
                let person = pass_args(verb, l, hint);
                assert!(
                    !person.iter().any(|a| a == atpkg::cli::DEFER_BUSY_FLIP_FLAG),
                    "{verb} {hint:?}: {person:?}"
                );
                let mut lane = lane_pass_args(verb, l, hint);
                lane.retain(|a| a != atpkg::cli::DEFER_BUSY_FLIP_FLAG);
                assert_eq!(
                    lane, person,
                    "the lane's argv is the person's plus the flag"
                );
            }
        }
    }
    assert_eq!(PassVerb::UpdateProgram("codex").to_string(), "update codex");
    assert_eq!(PassVerb::Update.to_string(), "update");
    assert!(!PassVerb::UpdateProgram("claude").refusal_is_a_failure());
}

#[test]
fn a_vendor_moved_trigger_maps_to_targeted_update_verbs() {
    assert_eq!(
        vendor_moved_verbs(&["claude", "codex"]),
        [
            PassVerb::UpdateProgram("claude"),
            PassVerb::UpdateProgram("codex")
        ]
    );
    assert!(vendor_moved_verbs(&[]).is_empty());
}

/// A 20 ms park over `layout`, with a fresh bump watch and the switch on.
fn park_20ms(layout: &atpkg::store::Layout, heads: Option<&mut VendorHeads>) -> ParkEnd {
    park_20ms_reading(layout, heads, &mut LiveSwitch::new(None, LoopGate::On))
}

/// [`park_20ms`] reading `[packages]` through `switch`.
fn park_20ms_reading(
    layout: &atpkg::store::Layout,
    heads: Option<&mut VendorHeads>,
    switch: &mut LiveSwitch,
) -> ParkEnd {
    let twenty = Duration::from_millis(20);
    sleep_interval_watching_bump(
        Some(layout),
        &mut (SystemTime::now() + twenty),
        twenty,
        &mut BumpWatch::default(),
        ParkProbes::NONE,
        heads,
        switch,
    )
}

#[test]
fn a_window_park_holds_the_watch_with_no_vendor_program_to_watch() {
    use atpkg::vendor_direct::watch::{Host, Watcher, watcher};
    let prefix =
        std::env::temp_dir().join(format!("aterm-head-watch-no-vendor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&prefix);
    std::fs::create_dir_all(&prefix).unwrap();
    let layout = atpkg::store::Layout { prefix };
    let get: Arc<ConcurrentVendorGetFn<'static>> =
        Arc::new(|_, _, _, _| panic!("no vendor program is installed, so no head is asked"));
    let mut heads: VendorHeads = (HeadWatch::hosted(&[], Host::Window), get);
    assert_eq!(watcher(&layout), Watcher::Nobody, "before the park");
    let end = park_20ms_reading(
        &layout,
        Some(&mut heads),
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(end, ParkEnd::Elapsed);
    assert_eq!(watcher(&layout), Watcher::Window, "the park took the claim");
    drop(heads);
    assert_eq!(
        watcher(&layout),
        Watcher::Nobody,
        "and it goes with the watch"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[test]
fn a_refused_window_claim_defers_its_held_flip_look() {
    use atpkg::quiet::{Due, DueFlip, RECHECK};
    use atpkg::vendor_direct::watch::Host;

    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: dir.path().to_path_buf(),
    };
    let held = crate::native_document_host::HeldAdvisoryLock::try_lock(
        std::fs::File::create(layout.prefix.join("vendor-head.hosts")).unwrap(),
    )
    .unwrap();
    let mut window = HeadWatch::hosted(&[], Host::Window);
    let mut watch = BumpWatch::default();
    let t0 = Instant::now();
    let looks = std::cell::Cell::new(0);
    let due = |_: &atpkg::store::Layout| {
        looks.set(looks.get() + 1);
        Some(DueFlip {
            why: Due::Quiet,
            checked: 1,
        })
    };
    assert!(!deferred_flip_due_if_watching(
        window.sit(&layout),
        &layout,
        &mut watch,
        t0,
        due,
    ));
    assert_eq!(looks.get(), 0, "the session still owns the store look");
    drop(held);
    assert!(!deferred_flip_due_if_watching(
        window.sit(&layout),
        &layout,
        &mut watch,
        t0,
        due,
    ));
    assert_eq!(looks.get(), 1, "the first free slice reads immediately");
    assert!(deferred_flip_due_if_watching(
        window.sit(&layout),
        &layout,
        &mut watch,
        t0 + RECHECK,
        due,
    ));
    assert_eq!(looks.get(), 2, "the second quiet look wakes the pass");
}

#[test]
fn a_window_does_not_repeat_a_sessions_completed_held_move() {
    use atpkg::quiet::{Deferral, Due, DueFlip, Recheck};
    use atpkg::vendor_direct::watch::{Host, Watcher, watcher};

    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::store::Layout {
        prefix: dir.path().to_path_buf(),
    };
    std::fs::create_dir_all(layout.build_dir("trust", 1)).unwrap();
    let record = Deferral {
        schema: 1,
        group: "rustc".into(),
        checked: 1_790_000_000,
        from: [("trust".into(), 1)].into(),
        ..Deferral::default()
    };
    let path = atpkg::quiet::record_path(&layout);
    std::fs::write(&path, aterm_toml::to_string(&record).unwrap()).unwrap();
    let mut recheck = Recheck::default();
    assert!(recheck.wake(Instant::now(), || Some(DueFlip {
        why: Due::Ceiling,
        checked: record.checked,
    })));
    let mut window = HeadWatch::hosted(&[], Host::Window);
    assert!(window.sit(&layout), "the window takes the shared watch");
    assert_eq!(watcher(&layout), Watcher::Window);

    let now = record.checked + 60;
    let own = OwnPasses::default();
    let mut why = PassWhy::FlipDue;
    assert!(
        scheduled_pass_gate_with_held_move(
            &mut why,
            Some(&layout),
            &recheck,
            &Default::default(),
            own,
            None,
            now,
        )
        .is_none(),
        "an unchanged pending move starts the whole pass"
    );
    assert_eq!(why, PassWhy::FlipDue);

    let in_flight = aterm_update_core::pkg_check::Stamps {
        in_flight: true,
        ..Default::default()
    };
    assert_eq!(
        scheduled_pass_gate_with_held_move(
            &mut why,
            Some(&layout),
            &recheck,
            &in_flight,
            own,
            None,
            now,
        )
        .map(|(_, then)| then),
        Some(PassWhy::FlipDue),
        "the window waits while the session's pass is in flight"
    );
    let completed = aterm_update_core::pkg_check::Stamps {
        last_success: Some(now),
        last_attempt: Some(now),
        ..Default::default()
    };
    std::fs::remove_file(&path).unwrap();
    assert!(
        scheduled_pass_gate_with_held_move(
            &mut why,
            Some(&layout),
            &recheck,
            &completed,
            own,
            None,
            now,
        )
        .is_some(),
        "a cleared move returns to the ordinary walk instead of launching"
    );
    assert_eq!(why, PassWhy::Walk);

    let renewed = Deferral {
        checked: record.checked + 1,
        ..record
    };
    std::fs::write(&path, aterm_toml::to_string(&renewed).unwrap()).unwrap();
    why = PassWhy::FlipDue;
    assert!(
        scheduled_pass_gate_with_held_move(
            &mut why,
            Some(&layout),
            &recheck,
            &completed,
            own,
            None,
            now,
        )
        .is_some(),
        "a new record must earn a new wake before it starts another pass"
    );
    assert_eq!(why, PassWhy::Walk);
}

#[cfg(target_os = "macos")]
#[test]
fn a_due_vendor_without_a_store_does_not_spin_the_package_lane() {
    fn thread_cpu() -> Duration {
        let mut stamp = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `stamp` is initialized and writable for this one call.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut stamp) },
            0
        );
        Duration::new(
            u64::try_from(stamp.tv_sec).unwrap(),
            u32::try_from(stamp.tv_nsec).unwrap(),
        )
    }

    let get: Arc<ConcurrentVendorGetFn<'static>> =
        Arc::new(|_, _, _, _| panic!("a missing store cannot fetch a vendor"));
    let mut heads: VendorHeads = (HeadWatch::new(&[]), get);
    let park = Duration::from_millis(100);
    let cpu_before = thread_cpu();
    let end = sleep_interval_watching_bump(
        None,
        &mut (SystemTime::now() + park),
        park,
        &mut BumpWatch::default(),
        ParkProbes::NONE,
        Some(&mut heads),
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(end, ParkEnd::Elapsed);
    assert!(
        thread_cpu().saturating_sub(cpu_before) < Duration::from_millis(15),
        "the missing-store park must not consume a core for its full wait"
    );
}

#[test]
fn slow_index_probe_does_not_hold_a_ready_vendor_hint() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    let mut state = model.init_state();
    let layout = store_with_legacy_claude("index-overlap");
    let asked = Arc::new(Mutex::new(Vec::new()));
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(vendor("2.1.281", Arc::clone(&asked)));
    let mut heads: VendorHeads = (HeadWatch::new(&[]), get);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut watch = BumpWatch {
        last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
        ..BumpWatch::default()
    };
    start_index_probe(&layout, &mut watch, true, Instant::now(), move |_| {
        release_rx.recv_timeout(Duration::from_secs(600)).unwrap();
        atpkg::index_probe::Probe::Published(77)
    });
    assert!(model.fire("Start", &mut state));
    assert!(watch.pending_index_probe.is_some());
    assert_eq!(state["phase"], 1);
    assert_eq!(modeled_index_owner(&watch), 1);
    let park = aterm_spec::wait::MUST_HAPPEN;
    set_park_slice_for_this_thread(park);
    let end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + park),
        park,
        &mut watch,
        ParkProbes::INDEX,
        Some(&mut heads),
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert!(model.fire("VendorReady", &mut state));
    assert!(model.fire("ChooseVendor", &mut state));
    assert_eq!(end, ParkEnd::VendorMoved(vec!["claude"]));
    assert_eq!(modeled_park_choice(&end), state["chosen"]);
    assert_eq!(asked.lock().unwrap().as_slice(), [None]);
    assert!(
        watch.pending_index_probe.is_some(),
        "one owned probe remains"
    );
    start_index_probe(
        &layout,
        &mut watch,
        true,
        Instant::now() + atpkg::index_probe::INTERVAL,
        |_| panic!("a second network worker must not start"),
    );
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !watch
        .pending_index_probe
        .as_ref()
        .is_some_and(|pending| pending.worker.is_finished())
        && Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert!(model.fire("FinishPublished", &mut state));
    assert_eq!(modeled_index_owner(&watch), 1, "ready still owned");
    assert!(model.fire("NextPark", &mut state));
    assert_eq!(take_ready_index_probe(&layout, &mut watch), Some(77));
    assert!(model.fire("Harvest", &mut state));
    assert!(model.fire("ChooseIndex", &mut state));
    assert_eq!(state["chosen"], 3);
    assert!(watch.pending_index_probe.is_none());
    assert_eq!(modeled_index_owner(&watch), 0);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[test]
fn a_suppressed_window_index_probe_uses_the_shared_expiry() {
    use aterm_models_atpkg::atpkg_index_shared_handoff_model;

    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join(format!(
            "aterm-window-index-shared-handoff-{}",
            std::process::id()
        )),
    };
    let model = atpkg_index_shared_handoff_model();
    let mut state = model.init_state();
    assert!(model.fire("Stamp", &mut state));
    for _ in 0..5 {
        assert!(model.fire("TickBeforeHandoff", &mut state));
    }
    let mut watch = BumpWatch::default();
    start_index_probe(&layout, &mut watch, true, Instant::now(), |_| {
        atpkg::index_probe::Probe::Suppressed(PARK_SLICE)
    });
    wait_for_what_must_happen("the suppressed worker completed", || {
        index_probe_answer_ready(&watch)
    });
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    assert!(model.fire("Handoff", &mut state));
    let completed = watch.last_index_probe.expect("worker completion");
    let expiry = completed + PARK_SLICE;
    assert_eq!(watch.shared_index_retry_at, Some(expiry));
    start_index_probe(
        &layout,
        &mut watch,
        true,
        expiry - Duration::from_nanos(1),
        |_| panic!("no early HEAD before the shared stamp expires"),
    );
    assert!(watch.pending_index_probe.is_none());
    assert!(model.fire("TickAfterHandoff", &mut state));
    let requests = Arc::new(AtomicU64::new(0));
    let asked = Arc::clone(&requests);
    start_index_probe(&layout, &mut watch, true, expiry, move |_| {
        asked.fetch_add(1, Ordering::SeqCst);
        atpkg::index_probe::Probe::Missing
    });
    assert!(model.fire("AtExpiry", &mut state));
    assert!(watch.pending_index_probe.is_some());
    wait_for_what_must_happen("the due worker completed", || {
        index_probe_answer_ready(&watch)
    });
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
}

#[test]
fn next_index_probe_is_paced_from_worker_completion() {
    let model = aterm_models_atpkg::atpkg_index_probe_completion_cadence_model();
    let mut state = model.init_state();
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join("aterm-index-completion-cadence-absent"),
    };
    let launched_at = Instant::now() - Duration::from_secs(5);
    let mut watch = BumpWatch {
        last_index_probe: Some(launched_at - atpkg::index_probe::INTERVAL),
        ..BumpWatch::default()
    };
    start_index_probe(&layout, &mut watch, true, launched_at, |_| {
        atpkg::index_probe::Probe::Missing
    });
    assert!(model.fire("Start", &mut state));
    assert!(model.fire("NetworkTick", &mut state));
    let completed_at = Arc::clone(
        &watch
            .pending_index_probe
            .as_ref()
            .expect("worker started")
            .completed_at,
    );
    wait_for_what_must_happen("the worker published its completion", || {
        watch
            .pending_index_probe
            .as_ref()
            .is_some_and(|pending| pending.done.load(Ordering::Acquire))
    });
    assert!(model.fire("Complete", &mut state));
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    let completion = *completed_at.get().expect("worker recorded completion");
    assert_eq!(watch.last_index_probe, Some(completion));

    let old_due = launched_at + atpkg::index_probe::INTERVAL;
    for _ in 0..5 {
        assert!(model.fire("Tick", &mut state));
    }
    assert!(model.fire("LocalCheck", &mut state));
    assert_eq!(state["phase"], 2);
    assert!(old_due < completion + atpkg::index_probe::INTERVAL);
    start_index_probe(&layout, &mut watch, true, old_due, |_| {
        panic!("start-based attempt would hit the fresh shared stamp")
    });
    assert!(watch.pending_index_probe.is_none());

    assert!(model.fire("Tick", &mut state));
    assert!(model.fire("DueCheck", &mut state));
    start_index_probe(
        &layout,
        &mut watch,
        true,
        completion + atpkg::index_probe::INTERVAL,
        |_| atpkg::index_probe::Probe::Missing,
    );
    assert_eq!(state["phase"], 3);
    assert!(watch.pending_index_probe.is_some(), "one real next probe");
    watch
        .pending_index_probe
        .take()
        .unwrap()
        .worker
        .join()
        .unwrap();
}

#[test]
fn near_index_wakes_while_unrelated_probe_helpers_are_still_running() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    let mut state = model.init_state();
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join(format!("aterm-near-index-early-{}", std::process::id())),
    };
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut watch = BumpWatch {
        last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
        ..BumpWatch::default()
    };
    start_index_probe_with_near_hint(&layout, &mut watch, true, Instant::now(), move |_, near| {
        near(44);
        release_rx.recv_timeout(FAKE_OUTWAITS_THE_TEST).unwrap();
        atpkg::index_probe::Probe::Published(49)
    });
    assert!(model.fire("Start", &mut state));
    wait_for_what_must_happen("the near hint arrived", || index_probe_answer_ready(&watch));
    assert!(
        !watch
            .pending_index_probe
            .as_ref()
            .unwrap()
            .worker
            .is_finished(),
        "the final answer is still blocked"
    );
    let end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + Duration::from_secs(60)),
        Duration::from_secs(60),
        &mut watch,
        ParkProbes::INDEX,
        None,
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(end, ParkEnd::IndexPublished(44));
    assert!(model.fire("NearPublished", &mut state));
    assert!(model.fire("ChooseIndex", &mut state));
    assert_eq!(modeled_park_choice(&end), state["chosen"]);
    assert_eq!(modeled_index_owner(&watch), 1);
    assert_eq!(
        watch.pending_index_probe.as_ref().unwrap().near_offered,
        Some(44)
    );
    assert!(
        !index_probe_answer_ready(&watch),
        "the delivered near hint must not spin the park"
    );

    assert!(model.fire("NextPark", &mut state));
    watch.note_full_pass_started();
    watch.unlanded = Some(44);
    assert!(model.fire("FullPass", &mut state));
    release_tx.send(()).unwrap();
    wait_for_what_must_happen("the released worker finished", || {
        watch
            .pending_index_probe
            .as_ref()
            .unwrap()
            .worker
            .is_finished()
    });
    assert!(model.fire("FinishPublished", &mut state));
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    assert!(model.fire("Harvest", &mut state));
    assert_eq!(watch.unlanded, Some(44));
    assert!(watch.pending_index_probe.is_none());
    assert!(model.check_invariant("NoStaleAnswerApplied", &state));
}

#[test]
fn near_index_unparks_the_vendor_head_wait_without_a_poll_tick() {
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir()
            .join(format!("aterm-near-index-unpark-{}", std::process::id())),
    };
    let (near_tx, near_rx) = std::sync::mpsc::channel();
    let (final_tx, final_rx) = std::sync::mpsc::channel();
    let mut watch = BumpWatch {
        last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
        ..BumpWatch::default()
    };
    start_index_probe_with_near_hint(
        &layout,
        &mut watch,
        true,
        Instant::now(),
        // The releases must come; each minute is a hang detector.
        move |_, near| {
            near_rx.recv_timeout(Duration::from_secs(60)).unwrap();
            near(44);
            final_rx.recv_timeout(Duration::from_secs(60)).unwrap();
            atpkg::index_probe::Probe::Missing
        },
    );
    let get: Arc<ConcurrentVendorGetFn<'static>> =
        Arc::new(|_, _, _, _| panic!("excluded vendor must not fetch"));
    let mut heads: VendorHeads = (
        HeadWatch::new(&[String::from("claude"), String::from("codex")]),
        get,
    );
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        near_tx.send(()).unwrap();
    });
    let park = aterm_spec::wait::MUST_HAPPEN;
    set_park_slice_for_this_thread(park);
    let end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + park),
        park,
        &mut watch,
        ParkProbes::INDEX,
        Some(&mut heads),
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(end, ParkEnd::IndexPublished(44), "woken by the hint");
    assert!(
        !watch
            .pending_index_probe
            .as_ref()
            .unwrap()
            .done
            .load(Ordering::Acquire)
    );
    release.join().unwrap();
    final_tx.send(()).unwrap();
    let pending = watch.pending_index_probe.take().unwrap();
    assert_eq!(
        pending.worker.join().unwrap(),
        atpkg::index_probe::Probe::Missing
    );
}

#[test]
fn final_index_answer_unparks_a_package_lane_without_vendor_heads() {
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir()
            .join(format!("aterm-final-index-unpark-{}", std::process::id())),
    };
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let mut watch = BumpWatch {
        last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
        ..BumpWatch::default()
    };
    start_index_probe_with_near_hint(&layout, &mut watch, true, Instant::now(), move |_, _| {
        finish_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        atpkg::index_probe::Probe::Published(45)
    });
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        finish_tx.send(()).unwrap();
    });
    let park = aterm_spec::wait::MUST_HAPPEN;
    set_park_slice_for_this_thread(park);
    let end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + park),
        park,
        &mut watch,
        ParkProbes::INDEX,
        None,
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(end, ParkEnd::IndexPublished(45), "woken by the answer");
    assert!(watch.pending_index_probe.is_none());
    release.join().unwrap();
}

#[test]
fn higher_near_index_after_pass_start_wakes_before_unrelated_helpers_finish() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    let mut state = model.init_state();
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join(format!(
            "aterm-near-index-after-pass-{}",
            std::process::id()
        )),
    };
    let _ = std::fs::remove_dir_all(&layout.prefix);
    std::fs::create_dir_all(&layout.prefix).unwrap();
    std::fs::write(layout.floor(), "43").unwrap();
    let (advance_tx, advance_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let mut watch = BumpWatch {
        last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
        ..BumpWatch::default()
    };
    start_index_probe_with_near_hint(&layout, &mut watch, true, Instant::now(), move |_, near| {
        near(44);
        advance_rx.recv_timeout(FAKE_OUTWAITS_THE_TEST).unwrap();
        near(45);
        finish_rx.recv_timeout(FAKE_OUTWAITS_THE_TEST).unwrap();
        atpkg::index_probe::Probe::Published(49)
    });
    assert!(model.fire("Start", &mut state));
    wait_for_what_must_happen("the near hint 44 arrived", || {
        index_probe_answer_ready(&watch)
    });
    assert_eq!(take_ready_index_probe(&layout, &mut watch), Some(44));
    for action in ["NearPublished", "ChooseIndex", "NextPark"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    watch.note_full_pass_started();
    std::fs::write(layout.floor(), "44").unwrap();
    watch.unlanded = Some(44);
    assert!(model.fire("FullPass", &mut state));
    assert!(!index_probe_answer_ready(&watch), "44 was already offered");
    advance_tx.send(()).unwrap();
    wait_for_what_must_happen("45 must wake before the final answer", || {
        index_probe_answer_ready(&watch)
    });
    assert!(
        !watch
            .pending_index_probe
            .as_ref()
            .unwrap()
            .worker
            .is_finished(),
        "the far/listing answer is still blocked"
    );
    assert_eq!(take_ready_index_probe(&layout, &mut watch), Some(45));
    for action in ["NearAfterPass", "ChooseIndex"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(model.check_invariant("NoStaleNearChosen", &state));
    assert!(
        !index_probe_answer_ready(&watch),
        "45 must not spin the park"
    );
    watch.note_full_pass_started();
    watch.unlanded = Some(45);
    finish_tx.send(()).unwrap();
    wait_for_what_must_happen("the released worker finished", || {
        watch
            .pending_index_probe
            .as_ref()
            .is_some_and(|pending| pending.worker.is_finished())
    });
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    assert_eq!(watch.unlanded, Some(45), "stale final49 must not clear it");
    assert!(watch.pending_index_probe.is_none());
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[test]
fn finished_stale_probe_keeps_newer_near_hint_unless_the_floor_landed_it() {
    for (label, landed, expected) in [("unlanded", 44, Some(45)), ("landed", 45, None)] {
        let layout = atpkg::store::Layout {
            prefix: std::env::temp_dir().join(format!(
                "aterm-near-index-finished-{label}-{}",
                std::process::id()
            )),
        };
        let _ = std::fs::remove_dir_all(&layout.prefix);
        std::fs::create_dir_all(&layout.prefix).unwrap();
        std::fs::write(layout.floor(), "43").unwrap();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let mut watch = BumpWatch {
            last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
            ..BumpWatch::default()
        };
        start_index_probe_with_near_hint(
            &layout,
            &mut watch,
            true,
            Instant::now(),
            move |_, near| {
                near(44);
                finish_rx.recv_timeout(FAKE_OUTWAITS_THE_TEST).unwrap();
                near(45);
                atpkg::index_probe::Probe::Published(49)
            },
        );
        wait_for_what_must_happen("the near hint 44 arrived", || {
            index_probe_answer_ready(&watch)
        });
        assert_eq!(take_ready_index_probe(&layout, &mut watch), Some(44));
        watch.note_full_pass_started();
        std::fs::write(layout.floor(), landed.to_string()).unwrap();
        watch.unlanded = Some(44);
        finish_tx.send(()).unwrap();
        wait_for_what_must_happen("the released worker finished", || {
            watch
                .pending_index_probe
                .as_ref()
                .is_some_and(|pending| pending.worker.is_finished())
        });
        assert!(index_probe_answer_ready(&watch));
        assert_eq!(
            take_ready_index_probe(&layout, &mut watch),
            expected,
            "{label}"
        );
        assert!(
            watch.pending_index_probe.is_none(),
            "{label}: worker joined"
        );
        assert_eq!(watch.unlanded, expected.map_or(Some(44), |_| None));
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }
}

#[test]
fn a_suppressed_near_build_does_not_hide_a_higher_near_build() {
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir()
            .join(format!("aterm-near-index-highwater-{}", std::process::id())),
    };
    let (advance_tx, advance_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut watch = BumpWatch {
        last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
        unlanded: Some(44),
        ..BumpWatch::default()
    };
    start_index_probe_with_near_hint(&layout, &mut watch, true, Instant::now(), move |_, near| {
        near(44);
        advance_rx.recv_timeout(FAKE_OUTWAITS_THE_TEST).unwrap();
        near(45);
        release_rx.recv_timeout(FAKE_OUTWAITS_THE_TEST).unwrap();
        atpkg::index_probe::Probe::Published(45)
    });
    wait_for_what_must_happen("the near hint 44 arrived", || {
        index_probe_answer_ready(&watch)
    });
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    assert_eq!(watch.unlanded, Some(44));
    assert!(
        !index_probe_answer_ready(&watch),
        "an old hint may not re-arm a zero-delay park"
    );
    advance_tx.send(()).unwrap();
    wait_for_what_must_happen("the near hint 45 arrived", || {
        index_probe_answer_ready(&watch)
    });
    assert!(
        !watch
            .pending_index_probe
            .as_ref()
            .unwrap()
            .worker
            .is_finished(),
        "the higher near hint precedes the worker's final answer"
    );
    assert_eq!(take_ready_index_probe(&layout, &mut watch), Some(45));
    assert_eq!(watch.unlanded, None);
    release_tx.send(()).unwrap();
    watch.note_full_pass_started();
    wait_for_what_must_happen("the released worker finished", || {
        watch
            .pending_index_probe
            .as_ref()
            .unwrap()
            .worker
            .is_finished()
    });
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    assert!(watch.pending_index_probe.is_none());
}

#[test]
fn stale_missing_index_answer_keeps_the_new_full_pass_mark() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    let mut state = model.init_state();
    let layout = store_with_legacy_claude("stale-index-after-pass");
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut watch = BumpWatch {
        last_index_probe: Some(Instant::now() - atpkg::index_probe::INTERVAL),
        ..BumpWatch::default()
    };
    start_index_probe(&layout, &mut watch, true, Instant::now(), move |_| {
        release_rx.recv_timeout(FAKE_OUTWAITS_THE_TEST).unwrap();
        atpkg::index_probe::Probe::Missing
    });
    assert!(model.fire("Start", &mut state));
    assert_eq!(modeled_index_owner(&watch), 1);

    std::fs::write(layout.bump_file(), "claude\n").unwrap();
    let end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + Duration::from_millis(20)),
        Duration::from_millis(20),
        &mut watch,
        ParkProbes::INDEX,
        None,
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(end, ParkEnd::Bumped);
    for action in ["BumpReady", "ChooseBump", "NextPark"] {
        assert!(model.fire(action, &mut state), "{action}");
    }
    assert_eq!(modeled_park_choice(&end), 1);
    std::fs::remove_file(layout.bump_file()).unwrap();

    watch.note_full_pass_started();
    watch.unlanded = Some(88);
    assert!(model.fire("FullPass", &mut state));
    assert_eq!(
        i64::try_from(watch.pass_generation).unwrap(),
        state["pass_generation"]
    );
    assert_eq!(modeled_index_owner(&watch), 1);
    start_index_probe(
        &layout,
        &mut watch,
        true,
        Instant::now() + atpkg::index_probe::INTERVAL,
        |_| panic!("an invalidated worker still owns the only probe slot"),
    );
    assert_eq!(modeled_index_owner(&watch), 1);
    release_tx.send(()).unwrap();
    wait_for_what_must_happen("the released worker finished", || {
        watch
            .pending_index_probe
            .as_ref()
            .is_some_and(|pending| pending.worker.is_finished())
    });
    assert!(model.fire("FinishMissing", &mut state));
    assert_eq!(take_ready_index_probe(&layout, &mut watch), None);
    assert!(model.fire("Harvest", &mut state));
    assert_eq!(i64::from(watch.unlanded.is_some()), state["unlanded"]);
    assert_eq!(watch.unlanded, Some(88));
    assert_eq!(modeled_index_owner(&watch), 0);

    let mut without_generation = BumpWatch {
        unlanded: Some(88),
        ..BumpWatch::default()
    };
    assert_eq!(
        index_probe_answer_wakes(&mut without_generation, atpkg::index_probe::Probe::Missing),
        None
    );
    assert_eq!(without_generation.unlanded, None);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[test]
fn a_ready_index_wins_before_a_ready_vendor_get() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    let mut state = model.init_state();
    let layout = store_with_legacy_claude("index-priority");
    let asked = Arc::new(Mutex::new(Vec::new()));
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(vendor("2.1.281", Arc::clone(&asked)));
    let mut heads: VendorHeads = (HeadWatch::new(&[]), get);
    let mut watch = BumpWatch {
        pending_index_probe: Some(PendingIndexProbe {
            worker: std::thread::spawn(|| atpkg::index_probe::Probe::Published(77)),
            pass_generation: 0,
            completed_at: Arc::new(std::sync::OnceLock::new()),
            near_build: Arc::new(AtomicU64::new(0)),
            done: Arc::new(AtomicBool::new(false)),
            near_offered: None,
        }),
        ..BumpWatch::default()
    };
    assert!(model.fire("Start", &mut state));
    wait_for_what_must_happen("the answering worker finished", || {
        watch
            .pending_index_probe
            .as_ref()
            .is_some_and(|pending| pending.worker.is_finished())
    });
    assert!(model.fire("FinishPublished", &mut state));
    std::fs::write(layout.bump_file(), "claude\n").unwrap();
    let bump_end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + Duration::from_millis(20)),
        Duration::from_millis(20),
        &mut watch,
        ParkProbes::INDEX,
        None,
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(bump_end, ParkEnd::Bumped);
    assert!(model.fire("BumpReady", &mut state));
    assert!(model.fire("ChooseBump", &mut state));
    assert_eq!(modeled_park_choice(&bump_end), state["chosen"]);
    assert!(model.fire("NextPark", &mut state));
    std::fs::remove_file(layout.bump_file()).unwrap();
    let full_end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() - Duration::from_secs(1)),
        Duration::from_secs(1),
        &mut watch,
        ParkProbes::INDEX,
        None,
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(full_end, ParkEnd::Elapsed);
    assert!(model.fire("FullDue", &mut state));
    assert!(model.fire("ChooseFull", &mut state));
    assert_eq!(modeled_park_choice(&full_end), state["chosen"]);
    assert!(model.fire("NextPark", &mut state));
    assert_eq!(
        sleep_interval_watching_bump(
            Some(&layout),
            &mut (SystemTime::now() + Duration::from_millis(20)),
            Duration::from_millis(20),
            &mut watch,
            ParkProbes::NONE,
            None,
            &mut LiveSwitch::new(None, LoopGate::On),
        ),
        ParkEnd::Elapsed
    );
    assert!(watch.pending_index_probe.is_some());
    assert!(model.fire("VendorReady", &mut state));
    let index_end = sleep_interval_watching_bump(
        Some(&layout),
        &mut (SystemTime::now() + Duration::from_secs(60)),
        Duration::from_secs(60),
        &mut watch,
        ParkProbes::INDEX,
        Some(&mut heads),
        &mut LiveSwitch::new(None, LoopGate::On),
    );
    assert_eq!(index_end, ParkEnd::IndexPublished(77));
    assert!(model.fire("Harvest", &mut state));
    assert!(model.fire("ChooseIndex", &mut state));
    assert_eq!(modeled_park_choice(&index_end), state["chosen"]);
    assert_ne!(
        modeled_park_choice(&ParkEnd::VendorMoved(vec!["claude"])),
        state["chosen"]
    );
    assert!(!model.action_enabled("ChooseVendor", &state));
    assert!(asked.lock().unwrap().is_empty(), "index wins before a GET");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[test]
fn a_vendor_hint_harvested_with_a_higher_priority_event_survives_to_the_next_park() {
    let model = aterm_models_atpkg::atpkg_index_pending_park_model();
    for (label, bumped, full_due, index_ready) in [
        ("bump", true, false, false),
        ("deadline", false, true, false),
        ("index", false, false, true),
    ] {
        let layout = store_with_legacy_claude(label);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let get: Arc<ConcurrentVendorGetFn<'static>> =
            Arc::new(vendor("2.1.281", Arc::clone(&asked)));
        let mut heads: VendorHeads = (HeadWatch::new(&[String::from("codex")]), get);
        let moved = heads.0.check(&layout, SystemTime::now(), heads.1.as_ref());
        assert_eq!(moved, ["claude"], "{label}: the head is actionable");
        let mut watch = BumpWatch::default();
        if index_ready {
            watch.pending_index_probe = Some(PendingIndexProbe {
                worker: std::thread::spawn(|| atpkg::index_probe::Probe::Published(77)),
                pass_generation: 0,
                completed_at: Arc::new(std::sync::OnceLock::new()),
                near_build: Arc::new(AtomicU64::new(0)),
                done: Arc::new(AtomicBool::new(false)),
                near_offered: None,
            });
            wait_for_what_must_happen(&format!("{label}: the answering worker finished"), || {
                watch
                    .pending_index_probe
                    .as_ref()
                    .is_some_and(|pending| pending.worker.is_finished())
            });
        }
        let end = finish_head_watch_slice(
            &layout,
            &mut heads.0,
            &mut watch,
            bumped,
            full_due,
            true,
            Some(moved),
        )
        .expect("higher-priority event");
        assert_eq!(
            end,
            if bumped {
                ParkEnd::Bumped
            } else if full_due {
                ParkEnd::Elapsed
            } else {
                ParkEnd::IndexPublished(77)
            },
            "{label} keeps priority"
        );

        let mut state = model.init_state();
        if index_ready {
            for action in ["Start", "FinishPublished", "Harvest"] {
                assert!(model.fire(action, &mut state), "{label}: {action}");
            }
        }
        assert!(model.fire("VendorReady", &mut state));
        let (ready, choose) = if bumped {
            (Some("BumpReady"), "ChooseBump")
        } else if full_due {
            (Some("FullDue"), "ChooseFull")
        } else {
            (None, "ChooseIndex")
        };
        if let Some(ready) = ready {
            assert!(model.fire(ready, &mut state), "{label}: {ready}");
        }
        assert!(model.fire(choose, &mut state), "{label}: {choose}");
        assert!(model.fire("NextPark", &mut state));
        assert_eq!(state["vendor"], 1, "{label}: model retains the hint");
        assert!(heads.0.due(SystemTime::now()), "{label}: real hint is due");

        let replay = sleep_interval_watching_bump(
            Some(&layout),
            &mut (SystemTime::now() + Duration::from_secs(2)),
            Duration::from_secs(2),
            &mut watch,
            ParkProbes::NONE,
            Some(&mut heads),
            &mut LiveSwitch::new(None, LoopGate::On),
        );
        assert_eq!(replay, ParkEnd::VendorMoved(vec!["claude"]), "{label}");
        assert!(model.fire("ChooseVendor", &mut state));
        assert_eq!(asked.lock().unwrap().len(), 1, "{label}: no second GET");
        assert_eq!(park_20ms(&layout, Some(&mut heads)), ParkEnd::Elapsed);
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }
}

#[test]
fn an_exclusion_edited_in_a_running_window_reaches_the_head_watch() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(vendor("2.1.281", Arc::clone(&asked)));
    let layout = store_with_legacy_claude("live-exclude");
    let config = layout.prefix.join("aterm.toml");
    std::fs::write(&config, "[packages]\nexclude = [\"claude\"]\n").unwrap();
    let mut switch = LiveSwitch::new(Some(config.clone()), LoopGate::On);
    let mut heads: VendorHeads = (HeadWatch::new(&[]), get);
    assert_eq!(
        park_20ms_reading(&layout, Some(&mut heads), &mut switch),
        ParkEnd::Elapsed
    );
    assert!(asked.lock().unwrap().is_empty(), "excluded: never asked");
    std::fs::write(&config, "[packages]\nexclude = []\n").unwrap();
    let heard = Instant::now() + MUST_HAPPEN;
    let mut end = ParkEnd::Elapsed;
    while !switch.exclude().is_empty() {
        assert!(
            Instant::now() < heard,
            "no slice read the edited file within a minute (a hang detector)"
        );
        end = park_20ms_reading(&layout, Some(&mut heads), &mut switch);
    }
    wait_for_what_must_happen("the slice that read the edit asked claude", || {
        !asked.lock().unwrap().is_empty()
    });
    let harvested = Instant::now() + MUST_HAPPEN;
    while matches!(end, ParkEnd::Elapsed) {
        assert!(
            Instant::now() < harvested,
            "the GET was not harvested within a minute (a hang detector)"
        );
        end = park_20ms_reading(&layout, Some(&mut heads), &mut switch);
    }
    assert_eq!(
        end,
        ParkEnd::VendorMoved(vec!["claude"]),
        "the negative control: included again, asked in the next slice"
    );
    assert_eq!(asked.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[test]
fn the_park_ends_at_a_wall_clock_deadline_and_never_strands_on_a_clock_set_back() {
    let layout = store_with_legacy_claude("wall");
    let five_minutes = Duration::from_secs(300);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let parked = layout.clone();
    std::thread::spawn(move || {
        let park = |until: SystemTime| {
            sleep_interval_watching_bump(
                Some(&parked),
                &mut { until },
                five_minutes,
                &mut BumpWatch::default(),
                ParkProbes::NONE,
                None,
                &mut LiveSwitch::new(None, LoopGate::On),
            )
        };
        let past = park(SystemTime::now() - Duration::from_secs(60));
        let set_back = park(SystemTime::now() + Duration::from_secs(3600));
        let _ = done_tx.send((past, set_back));
    });
    let (past, set_back) = done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("neither park sleeps toward its deadline");
    assert_eq!(past, ParkEnd::Elapsed);
    assert_eq!(
        set_back,
        ParkEnd::Elapsed,
        "an hour out, on a five-minute park: the clock went back"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[test]
fn a_moved_head_ends_the_park_with_its_programs() {
    fn park_until_moved(layout: &atpkg::store::Layout, heads: &mut VendorHeads) -> ParkEnd {
        let deadline = Instant::now() + MUST_HAPPEN;
        loop {
            let end = park_20ms(layout, Some(heads));
            if matches!(end, ParkEnd::VendorMoved(_)) || Instant::now() >= deadline {
                return end;
            }
            assert_eq!(end, ParkEnd::Elapsed);
        }
    }

    let asked = Arc::new(Mutex::new(Vec::new()));
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(vendor("2.1.281", Arc::clone(&asked)));
    let layout = store_with_legacy_claude("park");
    let mut heads: VendorHeads = (HeadWatch::new(&[]), Arc::clone(&get));
    assert_eq!(
        park_until_moved(&layout, &mut heads),
        ParkEnd::VendorMoved(vec!["claude"])
    );
    assert_eq!(asked.lock().unwrap().as_slice(), [None]);
    assert_eq!(park_20ms(&layout, Some(&mut heads)), ParkEnd::Elapsed);
    assert_eq!(asked.lock().unwrap().len(), 1, "resumed: not asked again");
    let mut due: VendorHeads = (HeadWatch::new(&[]), Arc::clone(&get));
    std::fs::write(layout.bump_file(), "claude\n").unwrap();
    assert_eq!(park_20ms(&layout, Some(&mut due)), ParkEnd::Bumped);
    assert_eq!(asked.lock().unwrap().len(), 1, "the bump first: no GET");
    std::fs::remove_file(layout.bump_file()).unwrap();
    assert_eq!(park_20ms(&layout, None), ParkEnd::Elapsed);
    assert_eq!(asked.lock().unwrap().len(), 1, "no watch: no GET");
    assert_eq!(
        park_until_moved(&layout, &mut due),
        ParkEnd::VendorMoved(vec!["claude"])
    );
    assert_eq!(asked.lock().unwrap().len(), 2);
    let empty = atpkg::store::Layout {
        prefix: layout.prefix.join("empty"),
    };
    std::fs::create_dir_all(&empty.prefix).unwrap();
    let mut heads: VendorHeads = (HeadWatch::new(&[]), Arc::clone(&get));
    assert_eq!(park_20ms(&empty, Some(&mut heads)), ParkEnd::Elapsed);
    assert_eq!(asked.lock().unwrap().len(), 2, "nothing installed: no GET");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[cfg(unix)]
#[test]
fn ready_codex_starts_its_pass_before_stalled_claude_finishes() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicBool, Ordering};

    let layout = store_with_legacy_claude("ready-codex");
    let codex_build = layout.build_dir("codex", 2_026_092_201);
    std::fs::create_dir_all(codex_build.join("bin")).unwrap();
    atpkg::atomic_symlink(&codex_build, &layout.program_current("codex")).unwrap();
    let argv_log = layout.prefix.join("argv");
    let atpkg = layout.prefix.join("atpkg-fake");
    std::fs::write(
        &atpkg,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n",
            argv_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&atpkg, std::fs::Permissions::from_mode(0o755)).unwrap();
    aterm_uds::deadline::output_until(
        &mut crate::qos::atpkg_command(&atpkg),
        std::time::Instant::now() + aterm_spec::wait::FIRST_EXEC,
    )
    .unwrap_or_else(|e| panic!("the stub's first exec within its hang detector: {e}"));
    std::fs::remove_file(&argv_log).expect("the warm run logged its argv");

    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let claude_finished = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&release);
    let finished = Arc::clone(&claude_finished);
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |program, url, _, _| {
        let bytes = if program == "claude" {
            let (lock, ready) = &*gate;
            let released = lock.lock().unwrap();
            let (released, _) = ready
                .wait_timeout_while(released, Duration::from_secs(600), |released| !*released)
                .unwrap();
            assert!(*released, "test releases Claude before its network timeout");
            finished.store(true, Ordering::Release);
            b"2.1.281".to_vec()
        } else {
            assert_eq!(program, "codex");
            br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec()
        };
        Ok(VendorGet::Body {
            bytes,
            etag: None,
            effective_url: url.to_string(),
        })
    });
    let mut head_watch = HeadWatch::new(&[]);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    assert_eq!(
        head_watch_slice(
            &mut head_watch,
            &layout,
            now,
            Some(Duration::ZERO),
            Duration::ZERO,
            &get,
            false,
        ),
        None
    );
    assert_eq!(head_watch.pending_count(), 2);
    let deadline = Instant::now() + MUST_HAPPEN;
    let moved = loop {
        head_watch.park_for_hint(Duration::from_millis(100));
        let moved = head_watch_slice(
            &mut head_watch,
            &layout,
            now + Duration::from_secs(1),
            Some(Duration::from_secs(1)),
            Duration::from_secs(1),
            &get,
            false,
        );
        if moved.is_some() || Instant::now() >= deadline {
            break moved;
        }
    };
    assert_eq!(moved, Some(vec!["codex"]));

    let mut lane = PkgLane {
        atpkg: atpkg.as_path(),
        child_path: |_| "".into(),
        layout: Some(&layout),
        private_state_root: false,
        post: |_| {},
        wait_row_open: false,
        backoff: ContentionBackoff::default(),
        ran_at: None,
        switch: LiveSwitch::new(None, LoopGate::On),
        ahead: false,
    };
    lane.run_vendor_moved(&["codex"], Some(&mut head_watch));
    assert_eq!(
        std::fs::read_to_string(&argv_log).unwrap(),
        format!(
            "update codex --head-watch --wait-lock 1800 --progress-file {}\n",
            layout.progress_file().display()
        )
    );
    assert!(
        !claude_finished.load(Ordering::Acquire),
        "the targeted pass started while Claude was stalled"
    );
    assert_eq!(head_watch.pending_count(), 1);

    let (lock, ready) = &*release;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    let deadline = Instant::now() + MUST_HAPPEN;
    let moved = loop {
        head_watch.park_for_hint(deadline.saturating_duration_since(Instant::now()));
        let moved = head_watch_slice(
            &mut head_watch,
            &layout,
            now + Duration::from_secs(2),
            Some(Duration::from_secs(1)),
            Duration::from_secs(1),
            &get,
            false,
        );
        if moved.is_some() || Instant::now() >= deadline {
            break moved;
        }
    };
    assert_eq!(moved, Some(vec!["claude"]));
    assert_eq!(head_watch.pending_count(), 0);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[cfg(unix)]
#[test]
fn a_targeted_pass_that_stood_aside_ends_the_trigger_on_the_tally() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("aterm-head-watch-lane-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let argv_log = dir.join("argv");
    let atpkg = dir.join("atpkg");
    let fake = |exit: u8| {
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexit {exit}\n",
            argv_log.display()
        );
        std::fs::write(&atpkg, script).unwrap();
        std::fs::set_permissions(&atpkg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_file(&argv_log);
    };
    let runs = || std::fs::read_to_string(&argv_log).unwrap_or_default();
    let mut lane = PkgLane {
        atpkg: atpkg.as_path(),
        child_path: |_| "".into(),
        layout: None,
        private_state_root: false,
        post: |_| {},
        wait_row_open: false,
        backoff: ContentionBackoff::default(),
        ran_at: None,
        switch: LiveSwitch::new(None, LoopGate::On),
        ahead: false,
    };
    fake(75);
    lane.run_vendor_moved(&["claude", "codex"], None);
    assert_eq!(
        runs(),
        "update claude --head-watch --wait-lock 1800\n",
        "codex is not queued behind the same holder"
    );
    assert_eq!(lane.backoff.cycles(), 1, "the wait is on the tally");
    fake(1);
    lane.run_vendor_moved(&["claude", "codex"], None);
    assert_eq!(
        runs(),
        "update claude --head-watch --wait-lock 1800\n\
         update codex --head-watch --wait-lock 1800\n",
        "passes that ran: every program"
    );
    assert_eq!(
        lane.backoff.cycles(),
        0,
        "a pass that ran starts the tally over"
    );
    for _ in 0..CONTENTION_WEDGE_CYCLES {
        let _ = lane.backoff.park(false);
    }
    assert!(lane.backoff.wedged());
    fake(1);
    lane.run_vendor_moved(&["claude"], None);
    assert_eq!(runs(), "", "the loop's own pass takes it");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_private_windows_passes_lead_with_the_flag_and_the_owners_do_not() {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(pass_lead(false), None);
    assert_eq!(pass_lead(true), Some("--private-state-root"));
    let dir = std::env::temp_dir().join(format!("aterm-private-lead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let argv_log = dir.join("argv");
    let atpkg = dir.join("atpkg");
    std::fs::write(
        &atpkg,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n",
            argv_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&atpkg, std::fs::Permissions::from_mode(0o755)).unwrap();
    for private_state_root in [true, false] {
        let _ = std::fs::remove_file(&argv_log);
        let mut lane = PkgLane {
            atpkg: atpkg.as_path(),
            child_path: |_| "".into(),
            layout: None,
            private_state_root,
            post: |_| {},
            wait_row_open: false,
            backoff: ContentionBackoff::default(),
            ran_at: None,
            switch: LiveSwitch::new(None, LoopGate::On),
            ahead: false,
        };
        for verb in [
            PassVerb::Seed,
            PassVerb::Update,
            PassVerb::UpdateProgram("codex"),
        ] {
            assert!(lane.run(verb, None).is_some(), "{verb}: the child ran");
        }
        let lead = if private_state_root {
            "--private-state-root "
        } else {
            ""
        };
        assert_eq!(
            std::fs::read_to_string(&argv_log).unwrap(),
            format!(
                "{lead}seed --wait-lock 1800\n\
                 {lead}update --defer-busy-flip --wait-lock 1800\n\
                 {lead}update codex --head-watch --wait-lock 1800\n"
            ),
            "private_state_root = {private_state_root}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn the_lane_holds_the_first_run_row_ahead_and_only_an_update_asks_the_login_path() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ASKED: AtomicUsize = AtomicUsize::new(0);
    fn login_path(first_run: bool) -> std::borrow::Cow<'static, str> {
        ASKED.fetch_add(1, Ordering::SeqCst);
        if first_run {
            "/first-run/bin:/usr/bin:/bin".into()
        } else {
            "/login/bin:/usr/bin:/bin".into()
        }
    }
    let dir = std::env::temp_dir().join(format!("aterm-first-run-ahead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let said = dir.join("said");
    let code = dir.join("code");
    let out = dir.join("out");
    std::fs::write(&code, "0\n").unwrap();
    let atpkg = dir.join("atpkg");
    let script = format!(
        "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"$PATH\" >> '{}'\nif [ -f '{}' ]; then cat '{}'; fi\n\
         read code < '{}'\nexit \"$code\"\n",
        said.display(),
        out.display(),
        out.display(),
        code.display()
    );
    std::fs::write(&atpkg, script).unwrap();
    std::fs::set_permissions(&atpkg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let posted = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&posted);
    let mut lane = PkgLane {
        atpkg: atpkg.as_path(),
        child_path: login_path,
        layout: None,
        private_state_root: false,
        post: move |e| sink.lock().unwrap().push(e),
        wait_row_open: false,
        backoff: ContentionBackoff::default(),
        ran_at: None,
        switch: LiveSwitch::new(None, LoopGate::On),
        ahead: true,
    };
    let rows = |posted: &Arc<Mutex<Vec<Wake>>>| -> Vec<&'static str> {
        posted
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|e| match e {
                Wake::PkgFirstRunAhead { open: true } => Some("open"),
                Wake::PkgFirstRunAhead { open: false } => Some("withdraw"),
                Wake::PkgPassEnded { .. } => Some("ended"),
                _ => None,
            })
            .collect()
    };
    lane.run(PassVerb::Seed, None);
    lane.run(PassVerb::Seed, None);
    assert_eq!(rows(&posted), ["open", "ended", "open", "ended"]);
    assert_eq!(
        ASKED.load(Ordering::SeqCst),
        0,
        "no seed asked the login shell"
    );
    lane.run(PassVerb::Update, None);
    assert_eq!(
        rows(&posted),
        ["ended"],
        "the update takes the row over silently"
    );
    assert!(!lane.ahead);
    assert_eq!(ASKED.load(Ordering::SeqCst), 1);
    let process_path = std::env::var("PATH").unwrap_or_default();
    assert_eq!(
        std::fs::read_to_string(&said).unwrap(),
        format!("seed {process_path}\nseed {process_path}\nupdate /first-run/bin:/usr/bin:/bin\n")
    );
    lane.ahead = true;
    lane.run(PassVerb::Seed, None);
    let _ = lane.park(
        Duration::ZERO,
        &mut BumpWatch::default(),
        false,
        false,
        None,
    );
    let _ = lane.park(
        Duration::ZERO,
        &mut BumpWatch::default(),
        false,
        false,
        None,
    );
    assert_eq!(rows(&posted), ["open", "ended", "withdraw"]);
    for exit in [
        atpkg::lock::CONTENDED_EXIT,
        aterm_update_core::pkg_check::PASS_OFFLINE_EXIT,
    ] {
        lane.ahead = true;
        std::fs::write(&code, format!("{exit}\n")).unwrap();
        lane.run(PassVerb::Update, None);
        assert_eq!(rows(&posted), ["open", "ended"], "exit {exit}");
        assert!(
            lane.ahead,
            "exit {exit}: the row is still the lane's to withdraw"
        );
        let _ = lane.park(
            Duration::ZERO,
            &mut BumpWatch::default(),
            false,
            false,
            None,
        );
        assert_eq!(rows(&posted), ["withdraw"], "exit {exit}");
    }
    lane.ahead = true;
    std::fs::write(&code, "1\n").unwrap();
    lane.run(PassVerb::Update, None);
    assert_eq!(rows(&posted), ["ended"]);
    assert!(!lane.ahead);
    let unusable = |body: &str| format!("atpkg: {}{body}\n", atpkg::cli::SEED_UNUSABLE_MARKER);
    let unserved = unusable(&format!(
        "ALab publishes no build {} (x86_64-apple-darwin) \u{2014} it builds for \
         aarch64-apple-darwin; nothing installs here until one is published",
        atpkg::cli::UNSERVED_ARCHITECTURE_CLAUSE
    ));
    let excluded = unusable(
        "[packages].exclude narrows the toolset to nothing installable on this machine \
         (aarch64-apple-darwin; excluded: ay, clean) \u{2014} shorten it to receive the toolset",
    );
    std::fs::write(&code, "2\n").unwrap();
    lane.ahead = true;
    std::fs::write(&out, &unserved).unwrap();
    lane.run(PassVerb::Update, None);
    assert_eq!(rows(&posted), ["open", "ended"], "an unserved architecture");
    assert!(lane.ahead, "the row is still the lane's to withdraw");
    let _ = lane.park(
        Duration::ZERO,
        &mut BumpWatch::default(),
        false,
        false,
        None,
    );
    assert_eq!(rows(&posted), ["withdraw"]);
    lane.ahead = true;
    std::fs::write(&out, &excluded).unwrap();
    lane.run(PassVerb::Update, None);
    assert_eq!(rows(&posted), ["ended"], "an exclusion that leaves nothing");
    assert!(!lane.ahead);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn only_the_first_run_a_person_waits_on_skips_the_login_path_and_the_clamp() {
    use std::os::unix::fs::PermissionsExt as _;
    fn update_path(waited: bool) -> std::borrow::Cow<'static, str> {
        if waited {
            "/first-run/bin:/usr/bin:/bin".into()
        } else {
            "/login/bin:/usr/bin:/bin".into()
        }
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let said = dir.path().join("said");
    let atpkg = dir.path().join("atpkg");
    std::fs::write(
        &atpkg,
        format!(
            "#!/bin/sh\nprintf '%s %s %s\\n' \"$1\" \"$PATH\" \"$(/bin/ps -o pri= -p $$ | /usr/bin/tr -d ' ')\" >> '{}'\n",
            said.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&atpkg, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fresh = atpkg::store::Layout {
        prefix: dir.path().join("fresh"),
    };
    let served = atpkg::store::Layout {
        prefix: dir.path().join("served"),
    };
    std::fs::create_dir_all(&served.prefix).unwrap();
    atpkg::status::write(
        &served,
        &atpkg::status::Status {
            schema: 1,
            last_success_at: "2023-11-14T22:13:20Z".to_string(),
            ..atpkg::status::Status::default()
        },
    )
    .unwrap();
    let stocked = atpkg::store::Layout {
        prefix: dir.path().join("stocked"),
    };
    let tool = stocked.build_dir("ay", 1971).join("bin").join("ay");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(&tool, "").unwrap();
    std::fs::create_dir_all(stocked.bin_dir()).unwrap();
    std::os::unix::fs::symlink(&tool, stocked.bin_dir().join("ay")).unwrap();
    for layout in [&fresh, &served, &stocked] {
        let mut lane = PkgLane {
            atpkg: atpkg.as_path(),
            child_path: update_path,
            layout: Some(layout),
            private_state_root: false,
            post: |_| {},
            wait_row_open: false,
            backoff: ContentionBackoff::default(),
            ran_at: None,
            switch: LiveSwitch::new(None, LoopGate::On),
            ahead: false,
        };
        assert!(lane.run(PassVerb::Update, None).is_some());
    }
    let said = std::fs::read_to_string(&said).unwrap();
    let runs: Vec<(&str, i32)> = said
        .lines()
        .map(|line| {
            let (head, pri) = line.rsplit_once(' ').expect(line);
            (head, pri.trim().parse().unwrap_or(-1))
        })
        .collect();
    assert_eq!(
        runs.iter().map(|(head, _)| *head).collect::<Vec<_>>(),
        [
            "update /first-run/bin:/usr/bin:/bin",
            "update /login/bin:/usr/bin:/bin",
            "update /login/bin:/usr/bin:/bin"
        ],
        "{said}"
    );
    if cfg!(target_os = "macos") {
        let pris: Vec<i32> = runs.iter().map(|(_, pri)| *pri).collect();
        assert!(pris[0] > 20, "the waited first run ran clamped: {said}");
        assert!(
            pris[1..].iter().all(|pri| (0..=20).contains(pri)),
            "a routine pass ran unclamped: {said}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_lane_thread_is_raised_while_it_holds_the_first_run_and_rests_after() {
    use crate::qos::{Role, class_for, class_of_self};
    use std::os::unix::fs::PermissionsExt as _;
    std::thread::spawn(|| {
        crate::qos::set_self(Role::Background);
        let dir = aterm_tempfile::tempdir().unwrap();
        let atpkg = dir.path().join("atpkg");
        std::fs::write(&atpkg, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&atpkg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut lane = PkgLane {
            atpkg: atpkg.as_path(),
            child_path: |_| "".into(),
            layout: None,
            private_state_root: false,
            post: |_| {},
            wait_row_open: false,
            backoff: ContentionBackoff::default(),
            ran_at: None,
            switch: LiveSwitch::new(None, LoopGate::On),
            ahead: true,
        };
        lane.hold_band();
        assert_eq!(
            class_of_self(),
            class_for(Role::Responsive),
            "raised at launch"
        );
        lane.run(PassVerb::Seed, None);
        assert_eq!(
            class_of_self(),
            class_for(Role::Responsive),
            "across its seed"
        );
        lane.run(PassVerb::Update, None);
        assert!(!lane.ahead, "a failed update takes the row over");
        assert_eq!(
            class_of_self(),
            class_for(Role::Background),
            "rests after it"
        );
        lane.ahead = true;
        lane.hold_band();
        lane.drop_ahead();
        assert_eq!(
            class_of_self(),
            class_for(Role::Background),
            "rests when a park withdraws the row"
        );
    })
    .join()
    .expect("the lane thread's assertions");
}

#[test]
fn lane_posts_before_the_loop_exists_are_delivered_when_it_goes_live() {
    use std::sync::Arc;
    let sink = LaneSink::new();
    let early = sink.clone();
    early.send_event(Wake::PkgFirstRunAhead { open: true });
    early.send_event(Wake::PkgPassEnded { clean: true });
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&delivered);
    sink.go_live(move |event| {
        seen.lock().unwrap().push(match event {
            Wake::PkgFirstRunAhead { open } => format!("ahead {open}"),
            Wake::PkgPassEnded { clean } => format!("ended {clean}"),
            _ => "other".to_string(),
        });
        true
    });
    assert_eq!(*delivered.lock().unwrap(), ["ahead true", "ended true"]);
    early.send_event(Wake::PkgFirstRunAhead { open: false });
    assert_eq!(
        *delivered.lock().unwrap(),
        ["ahead true", "ended true", "ahead false"]
    );
}

#[test]
fn a_reader_posting_through_the_sink_is_accepted_then_answered_by_the_loop() {
    use crate::spawn::WakeTarget as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    let sink = LaneSink::new();
    let post = sink.wake_post();
    let output = || Wake::Output {
        session: 0,
        window: WindowId(0),
    };
    assert!(post(output()), "a queued post is accepted");
    let (open, delivered) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicU32::new(0)));
    let (loop_open, loop_got) = (Arc::clone(&open), Arc::clone(&delivered));
    sink.go_live(move |_| {
        loop_got.fetch_add(1, Ordering::SeqCst);
        loop_open.load(Ordering::SeqCst)
    });
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        1,
        "the queued wake, at go-live"
    );
    assert!(post(output()), "the live loop took it");
    open.store(false, Ordering::SeqCst);
    assert!(!post(output()), "the loop is gone, and the reader hears so");
    assert_eq!(delivered.load(Ordering::SeqCst), 3);
}

#[test]
fn the_first_run_row_opens_ahead_only_where_the_seed_will_adopt() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let fresh = atpkg::store::Layout {
        prefix: dir.path().join("fresh"),
    };
    let consenting = atpkg::config::PackagesConfig::default();
    let off = atpkg::config::PackagesConfig {
        auto_install: Some(false),
        ..atpkg::config::PackagesConfig::default()
    };
    let armed = atpkg::manager_enabled();
    assert_eq!(first_run_ahead(Some(&fresh), &consenting), armed);
    assert!(!first_run_ahead(Some(&fresh), &off));
    assert!(!first_run_ahead(None, &consenting));
    let declined = atpkg::store::Layout {
        prefix: dir.path().join("declined"),
    };
    std::fs::create_dir_all(&declined.prefix).unwrap();
    std::fs::write(declined.declined(), "# declined\n").unwrap();
    assert!(!first_run_ahead(Some(&declined), &consenting));
    let served = atpkg::store::Layout {
        prefix: dir.path().join("served"),
    };
    std::fs::create_dir_all(&served.prefix).unwrap();
    atpkg::status::write(
        &served,
        &atpkg::status::Status {
            schema: 1,
            last_success_at: "2023-11-14T22:13:20Z".to_string(),
            ..atpkg::status::Status::default()
        },
    )
    .unwrap();
    assert!(!first_run_ahead(Some(&served), &consenting));
    let stocked = atpkg::store::Layout {
        prefix: dir.path().join("stocked"),
    };
    let tool = stocked.build_dir("ay", 1971).join("bin").join("ay");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(&tool, "").unwrap();
    std::fs::create_dir_all(stocked.bin_dir()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&tool, stocked.bin_dir().join("ay")).unwrap();
    #[cfg(unix)]
    assert!(!first_run_ahead(Some(&stocked), &consenting));
}

#[cfg(unix)]
#[test]
fn a_pass_is_a_first_run_only_on_an_empty_store_and_reports_its_exit() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::{Arc, Mutex};
    let dir = std::env::temp_dir().join(format!("aterm-first-run-lane-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let atpkg = dir.join("atpkg");
    let fake = |stdout: &str, exit: u8| {
        let script = format!("#!/bin/sh\nprintf '%s\\n' '{stdout}'\nexit {exit}\n");
        std::fs::write(&atpkg, script).unwrap();
        std::fs::set_permissions(&atpkg, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let announce = format!(
        "atpkg: {}installing 10 program(s) over the network (about 3 GB)",
        atpkg::cli::NET_STARTING_MARKER
    );
    let empty = atpkg::store::Layout {
        prefix: dir.join("empty"),
    };
    std::fs::create_dir_all(&empty.prefix).unwrap();
    let stocked = atpkg::store::Layout {
        prefix: dir.join("stocked"),
    };
    let tool = stocked.build_dir("ay", 1971).join("bin").join("ay");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(&tool, "").unwrap();
    std::fs::create_dir_all(stocked.bin_dir()).unwrap();
    std::os::unix::fs::symlink(&tool, stocked.bin_dir().join("ay")).unwrap();
    assert!(pass_is_first_run(None), "no store: nothing of it is here");
    assert!(pass_is_first_run(Some(&empty)));
    assert!(!pass_is_first_run(Some(&stocked)));
    let agents_only = atpkg::store::Layout {
        prefix: dir.join("agents-only"),
    };
    let agent = agents_only
        .build_dir("claude", 7)
        .join("bin")
        .join("claude");
    std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
    std::fs::write(&agent, "").unwrap();
    std::fs::create_dir_all(agents_only.bin_dir()).unwrap();
    std::os::unix::fs::symlink(&agent, agents_only.bin_dir().join("claude")).unwrap();
    assert!(
        pkg_store_holds_programs(Some(&agents_only)),
        "not vacuous: the store holds a live claude"
    );
    assert!(
        pass_is_first_run(Some(&agents_only)),
        "claude alone: still the first run"
    );
    let drive = |layout: &atpkg::store::Layout, verb: PassVerb| {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&posted);
        let mut lane = PkgLane {
            atpkg: atpkg.as_path(),
            child_path: |_| "".into(),
            layout: Some(layout),
            private_state_root: false,
            post: move |e| sink.lock().unwrap().push(e),
            wait_row_open: false,
            backoff: ContentionBackoff::default(),
            ran_at: None,
            switch: LiveSwitch::new(None, LoopGate::On),
            ahead: false,
        };
        lane.run(verb, None).expect("the child ran");
        std::mem::take(&mut *posted.lock().unwrap())
    };
    let started = |posted: &[Wake]| {
        posted.iter().find_map(|e| match e {
            Wake::PkgSeedStarted { first_run, .. } => Some(*first_run),
            _ => None,
        })
    };
    let ended = |posted: &[Wake]| {
        posted.iter().find_map(|e| match e {
            Wake::PkgPassEnded { clean } => Some(*clean),
            _ => None,
        })
    };
    fake(&announce, 0);
    let first = drive(&empty, PassVerb::Update);
    assert_eq!(started(&first), Some(true), "an empty store: the first run");
    assert_eq!(
        ended(&first),
        Some(true),
        "exit 0, no failure marker: clean"
    );
    let routine = drive(&stocked, PassVerb::Update);
    assert_eq!(started(&routine), Some(false), "a stocked store: routine");
    assert_eq!(ended(&routine), Some(true));
    fake(&announce, 1);
    assert_eq!(
        ended(&drive(&stocked, PassVerb::Update)),
        Some(false),
        "exit 1"
    );
    fake(
        &format!(
            "atpkg: {}network provisioning installed nothing",
            atpkg::cli::NET_FAILED_MARKER
        ),
        0,
    );
    assert_eq!(
        ended(&drive(&stocked, PassVerb::Update)),
        Some(false),
        "a failure marker at exit 0 is not clean"
    );
    fake("atpkg: codex 0.157.0 refused: SHA256SUMS disagrees", 1);
    let refused = drive(&stocked, PassVerb::UpdateProgram("codex"));
    assert!(
        refused.iter().all(|e| matches!(
            e,
            Wake::PkgPassEnded { clean: false } | Wake::PkgProgress { .. }
        )),
        "no marker, so nothing that could raise a row: {refused:?}"
    );
    fake("", 0);
    assert_eq!(
        ended(&drive(&stocked, PassVerb::UpdateProgram("codex"))),
        Some(false),
        "a targeted pass never clears a whole pass's failure"
    );
    fake(&announce, 0);
    assert_eq!(
        started(&drive(&agents_only, PassVerb::Update)),
        Some(true),
        "the lane tags a claude-only store's pass a first run"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_whole_pass_that_cannot_launch_is_a_failure() {
    use std::sync::{Arc, Mutex};
    let missing =
        std::env::temp_dir().join(format!("aterm-no-such-atpkg-{}/atpkg", std::process::id()));
    for (verb, failure) in [
        (PassVerb::Seed, true),
        (PassVerb::Update, true),
        (PassVerb::UpdateProgram("codex"), false),
    ] {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&posted);
        let mut lane = PkgLane {
            atpkg: missing.as_path(),
            child_path: |_| "".into(),
            layout: None,
            private_state_root: false,
            post: move |e| sink.lock().unwrap().push(e),
            wait_row_open: false,
            backoff: ContentionBackoff::default(),
            ran_at: None,
            switch: LiveSwitch::new(None, LoopGate::On),
            ahead: false,
        };
        assert!(lane.run(verb, None).is_none(), "{verb}: nothing ran");
        let posted = std::mem::take(&mut *posted.lock().unwrap());
        let failed = posted.iter().any(|e| {
            matches!(e, Wake::PkgSeedFailed { detail }
                if detail.starts_with(&format!("could not launch atpkg {verb}: ")))
        });
        assert_eq!(failed, failure, "{verb}: {posted:?}");
        assert!(
            posted
                .iter()
                .any(|e| matches!(e, Wake::PkgPassEnded { clean: false })),
            "{verb}: {posted:?}"
        );
    }
}

#[test]
fn an_informational_unusable_marker_is_no_failure() {
    let unusable = |detail: &str| Wake::PkgSeedUnusable {
        detail: detail.to_string(),
    };
    assert!(!seed_event_is_failure(&unusable(
        "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
         — it builds for aarch64-apple-darwin; nothing installs here until one is published"
    )));
    assert!(!seed_event_is_failure(&unusable(
        "every ALab program available here was removed on this machine (ay, trust) — \
         nothing to install. `aterm pkg install <program>` brings one back"
    )));
    assert!(seed_event_is_failure(&unusable(
        "[packages].exclude narrows the toolset to nothing installable on this machine \
         (aarch64-apple-darwin; excluded: ay, clean) — shorten it to receive the toolset"
    )));
    assert!(seed_event_is_failure(&Wake::PkgSeedFailed {
        detail: "nothing could be installed".into()
    }));
}

#[test]
fn only_the_announcement_carries_the_first_run_tag() {
    let tagged = tag_first_run(
        Wake::PkgSeedStarted {
            detail: "installing 1".into(),
            first_run: false,
        },
        true,
    );
    assert!(matches!(
        tagged,
        Wake::PkgSeedStarted {
            first_run: true,
            ..
        }
    ));
    let other = tag_first_run(
        Wake::PkgSeedDone {
            detail: "ok".into(),
        },
        true,
    );
    assert!(matches!(other, Wake::PkgSeedDone { .. }));
}

#[test]
fn the_slice_asks_when_due_and_twenty_seconds_after_a_wake() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(vendor("2.1.281", Arc::clone(&asked)));
    let layout = store_with_legacy_claude("slice");
    let mut heads = HeadWatch::new(&[String::from("codex")]);
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let five = Duration::from_secs(5);
    let slice = |heads: &mut HeadWatch, at: SystemTime, wall: Duration| {
        head_watch_slice(heads, &layout, at, Some(wall), five, &get, false)
    };
    assert_eq!(
        slice(&mut heads, t0, five),
        None,
        "launched the head worker"
    );
    heads.park_for_hint(MUST_HAPPEN);
    assert_eq!(
        slice(&mut heads, t0 + Duration::from_secs(1), five),
        Some(vec!["claude"])
    );
    assert_eq!(slice(&mut heads, t0 + five, five), None, "not due");
    let woke = t0 + Duration::from_secs(60);
    assert_eq!(slice(&mut heads, woke, Duration::from_secs(55)), None);
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "the wake waits 20 s for the network"
    );
    assert_eq!(
        slice(&mut heads, woke + Duration::from_secs(20), five),
        None
    );
    heads.park_for_hint(MUST_HAPPEN);
    assert_eq!(
        slice(&mut heads, woke + Duration::from_secs(21), five),
        None,
        "a 304 is harvested without a moved head"
    );
    assert_eq!(
        asked.lock().unwrap().as_slice(),
        [None, Some(String::from("\"2.1.281\""))],
        "asked again after the wake, conditionally: a 304 is nothing"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}
