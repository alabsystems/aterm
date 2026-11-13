// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Config;

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn path_feed_fingerprints_track_referenced_file_content() {
    let dir = std::env::temp_dir().join(format!("aterm-feed-fp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lex = dir.join("lexicon.toml");
    std::fs::write(&lex, "one").unwrap();
    let config = cfg(&format!(
        "[sparkle_words]\nlexicon = {}\n",
        super::toml_basic_string(&lex.to_string_lossy())
    ));
    let base = config.path_feed_fingerprints();
    assert_eq!(
        base,
        config.path_feed_fingerprints(),
        "untouched files fingerprint stably"
    );
    std::fs::write(&lex, "two").unwrap();
    let lexed = config.path_feed_fingerprints();
    assert_ne!(lexed.deco, base.deco, "lexicon content feeds deco");
    std::fs::remove_file(&lex).unwrap();
    assert_ne!(
        config.path_feed_fingerprints().deco,
        lexed.deco,
        "a vanished lexicon file reads as changed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn path_feed_generation_binds_consumer_and_fingerprint_to_one_read() {
    use std::hash::Hasher as _;

    let dir = std::env::temp_dir().join(format!("aterm-feed-generation-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lexicon = dir.join("lexicon.toml");
    let v1 = "# generation-v1\n";
    let v2 = "# generation-v2\n";
    std::fs::write(&lexicon, v1).unwrap();
    let lexicon_path = lexicon.to_string_lossy().into_owned();
    let config = cfg(&format!("[sparkle_words]\nlexicon = {lexicon_path:?}\n"));
    let model = aterm_models_misc::path_feed_snapshot_model();
    let mut model_state = model.init_state();
    let v1_fingerprint = config.path_feed_fingerprints().deco;

    let mut reads = 0;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let (_, admitted) = super::read_and_fingerprint_path_feed_with_reader(
        &lexicon_path,
        aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
        &mut hasher,
        |admitted_path, max_bytes| {
            reads += 1;
            let admitted =
                aterm_effects::file_feed::read_bounded_regular_utf8(admitted_path, max_bytes);
            std::fs::write(&lexicon, v2).unwrap();
            admitted
        },
    );
    let admitted_fingerprint = hasher.finish();
    for action in ["Read", "LiveMutate", "Publish"] {
        assert!(
            model.fire(action, &mut model_state),
            "{action}: {model_state:?}"
        );
    }
    assert!(model.check_invariant("PublishedPairComesFromAdmittedRead", &model_state));
    assert_eq!(model_state["prepared"], 1);
    assert_eq!(model_state["fingerprint"], 1);
    assert_eq!(model_state["live"], 2);
    assert_eq!(reads, 1, "the configured path is opened exactly once");
    assert_eq!(
        admitted.as_deref(),
        Ok(v1),
        "the consumer holds the admitted v1"
    );
    assert_eq!(
        admitted_fingerprint, v1_fingerprint,
        "Tier-1 projection: prepared=1 and fingerprint=1 derive from the admitted v1 String"
    );
    assert_ne!(
        admitted_fingerprint,
        config.path_feed_fingerprints().deco,
        "Tier-1 negative control: the v1 consumer never carries the live v2 fingerprint"
    );

    let replacement = config.prepare_path_feed_generation();
    assert_eq!(
        replacement.fingerprints,
        config.path_feed_fingerprints(),
        "the next generation admits and identifies the replacement bytes"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn disabled_sparkle_performs_no_path_feed_reads() {
    let dir = std::env::temp_dir().join(format!("aterm-disabled-feed-{}", std::process::id()));
    let lexicon = dir.join("lexicon.toml");
    let toy = dir.join("toy.toml");
    let disabled = cfg(&format!(
        "[sparkle_words]\nenabled = false\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
        lexicon.to_string_lossy(),
        toy.to_string_lossy(),
    ));

    super::reset_path_feed_read_count();
    let prepared = disabled.prepare_path_feed_generation();
    assert_eq!(super::path_feed_read_count(), 0);
    assert_eq!(prepared.sparkle.consumer_capabilities(), Default::default());

    let enabled = cfg(&format!(
        "[sparkle_words]\nenabled = true\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
        lexicon.to_string_lossy(),
        toy.to_string_lossy(),
    ));
    super::reset_path_feed_read_count();
    let _ = enabled.prepare_path_feed_generation();
    assert_eq!(
        super::path_feed_read_count(),
        2,
        "negative control: enabling the same two configured feeds admits each once"
    );
}

#[test]
fn the_launch_generation_lands_the_same_kitty_vocabulary_after_the_join() {
    let config = cfg("[sparkle_words]\nlanguages = [\"en\", \"es\"]\n");
    let reload = config.prepare_path_feed_generation();
    let held = super::TRICKS_COMPILE_GATE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (handed_tx, handed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = handed_tx.send(config.prepare_launch_path_feed_generation());
    });
    let launch = handed
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the launch generation is handed over while its vocabulary compiles");
    let mut scratch = String::new();
    assert_eq!(
        launch.sparkle.tricks().classify("sit", &mut scratch),
        None,
        "handed over before its vocabulary landed: a press reads the empty one"
    );
    drop(held);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the launch generation's kitty vocabulary to land",
        || launch.sparkle.0.tricks.built.get().map(|_| ()),
    );
    assert!(
        reload
            .sparkle
            .tricks()
            .classify("sit", &mut scratch)
            .is_some(),
        "negative control: a compiled vocabulary knows `sit`"
    );
    for word in ["sit", "kitty", "please", "siéntate", "sientate", "compile"] {
        assert_eq!(
            launch.sparkle.tricks().classify(word, &mut scratch),
            reload.sparkle.tricks().classify(word, &mut scratch),
            "{word}"
        );
    }
    assert_eq!(
        launch.sparkle.tricks().surface_count(),
        reload.sparkle.tricks().surface_count()
    );
}

/// A slot shares a table of the same languages while a generation holds it, builds another for
/// other languages, and keeps nothing once none does. The slot is this test's own, so no other
/// thread's generation can take it.
#[test]
fn a_vocabulary_slot_shares_what_a_generation_holds_and_keeps_nothing_after() {
    let slot: super::TableSlot<usize> = std::sync::Mutex::new(None);
    let (en, es) = (["en".to_string()], ["es".to_string()]);
    let builds = std::cell::Cell::new(0);
    let build = || {
        builds.set(builds.get() + 1);
        builds.get()
    };
    let (first, shared) = super::shared_table(&slot, &en, build);
    assert!(!shared, "an empty slot shares nothing");
    let (second, shared) = super::shared_table(&slot, &en, build);
    assert!(shared, "a held table is shared");
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "the second generation holds the first's table"
    );
    assert_eq!(builds.get(), 1, "a shared table is not built again");

    let table = std::sync::Arc::downgrade(&first);
    drop((first, second));
    assert!(
        table.upgrade().is_none(),
        "the slot keeps a table no generation holds"
    );
    let (rebuilt, shared) = super::shared_table(&slot, &en, build);
    assert!(!shared, "a table no generation holds is rebuilt");
    assert_eq!(builds.get(), 2, "a table no generation holds is rebuilt");

    let (other, shared) = super::shared_table(&slot, &es, build);
    assert!(!shared, "other languages share nothing");
    assert!(!std::sync::Arc::ptr_eq(&rebuilt, &other));
    assert_eq!(builds.get(), 3, "other languages build their own table");
}

/// Each generation logs the lexicon's warnings whether it built the tables or shares them (those
/// lines read this generation's `cjk_single_char`), and the real slots keep nothing once no
/// generation holds the tables. The language is this test's own, so no other generation can hold
/// them; another thread's generation may take the slot between the two here, which only makes
/// the second compile, so that the two share is not held here but by
/// [`a_vocabulary_slot_shares_what_a_generation_holds_and_keeps_nothing_after`]. The vocabulary's
/// own lines are not read here: they come of malformed embedded data alone, which
/// `aterm_lexicon`'s suite holds to none, so no language raises one.
#[test]
fn each_generation_logs_its_vocabulary_warnings_and_no_slot_outlives_them() {
    let config = cfg("[sparkle_words]\nenabled = true\nlanguages = [\"zz-this-test\"]\n");
    let lines = || super::CONFLICT_LINES.with(|lines| lines.replace(0));
    let _ = lines();
    let first = config.prepare_sparkle_runtime();
    let logged = lines();
    assert!(logged >= 1, "an unknown language warns in the lexicon");
    let second = config.prepare_sparkle_runtime();
    assert_eq!(
        lines(),
        logged,
        "the second generation logs what the first did"
    );
    let tricks = std::sync::Arc::downgrade(&second.0.tricks.built);
    let lexicon =
        std::sync::Arc::downgrade(&second.resolved().expect("sparkle is enabled").lexicon);
    drop((first, second));
    assert!(
        tricks.upgrade().is_none() && lexicon.upgrade().is_none(),
        "a slot keeps a table no generation holds"
    );
}

#[test]
fn an_unlanded_kitty_vocabulary_classifies_nothing() {
    let unlanded = super::TrickVocabulary {
        built: std::sync::Arc::default(),
        lazy_langs: None,
    };
    let mut scratch = String::new();
    assert_eq!(unlanded.get().classify("sit", &mut scratch), None);
    assert_eq!(unlanded.get().surface_count(), 0);
    assert!(
        super::TrickVocabulary::compiled(vec!["en".to_string()])
            .get()
            .classify("sit", &mut scratch)
            .is_some(),
        "negative control: the compiled vocabulary knows `sit`"
    );
}

#[test]
fn path_feed_fingerprints_enforce_the_loader_specific_caps() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-feed-caps-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch dir");
    let lexicon = dir.path().join("lexicon.toml");
    let toy = dir.path().join("toy.toml");
    let config = cfg(&format!(
        "[sparkle_words]\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
        lexicon.to_string_lossy(),
        toy.to_string_lossy(),
    ));
    let missing = config.path_feed_fingerprints();

    std::fs::write(
        &lexicon,
        vec![b'x'; aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1],
    )
    .unwrap();
    std::fs::write(
        &toy,
        vec![b'x'; aterm_effects::spec::MAX_TOY_PACK_BYTES + 1],
    )
    .unwrap();
    assert_eq!(
        config.path_feed_fingerprints(),
        missing,
        "oversized inputs are rejected exactly like the loaders, not hashed past their caps"
    );

    std::fs::write(&lexicon, "# admitted lexicon\n").unwrap();
    std::fs::write(&toy, "# admitted toy manifest bytes\n").unwrap();
    let admitted = config.path_feed_fingerprints();
    assert_ne!(admitted.deco, missing.deco);
}

#[cfg(unix)]
#[test]
fn path_feed_fingerprint_and_loaders_refuse_a_writerless_fifo() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-feed-fifo-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch dir");
    let path = dir.path().join("feed.fifo");
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("FIFO path");
    // SAFETY: `path_c` is a live NUL-terminated pathname and mkfifo retains no pointer. The path is inside a unique tempdir owned by this test.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    let config = cfg(&format!(
        "[sparkle_words]\nlexicon = {0:?}\ntoy_packs = [{0:?}]\n",
        path.to_string_lossy(),
    ));
    let fifo_fingerprint = config.path_feed_fingerprints();
    let _ = config.sparkle_runtime_parts();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        config.path_feed_fingerprints(),
        fifo_fingerprint,
        "a FIFO and a missing file are both rejected admissions"
    );
}

#[test]
fn byte_equal_reload_still_rereads_touched_path_feed_files() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-feed-touch-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch dir");
    let lex = dir.path().join("lexicon.toml");
    std::fs::write(&lex, "# lexicon v1\n").unwrap();

    let mut app = crate::App::headless_for_test();
    app.config = cfg(&format!(
        "[sparkle_words]\nlexicon = {}\n",
        super::toml_basic_string(&lex.to_string_lossy())
    ));
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.path_feed_fps = app.config.path_feed_fingerprints();
    app.recompute_sparkle();
    assert!(!app.sparkle_dirty, "recompute clears the dirty latch");

    let (deco_cfg, lexicon) = {
        let r = app.sparkle.as_ref().expect("sparkle resolves on defaults");
        (r.cfg.clone(), r.lexicon.clone())
    };
    let wid = crate::WindowId(0);
    let seed_scan = |app: &mut crate::App| {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.fx.decos_mut().rescan_from_cells(
            &[],
            &[],
            0,
            0,
            &lexicon,
            &deco_cfg,
            7,
            std::time::Instant::now(),
        );
        assert!(
            !ws.fx.decos().needs_rescan(7),
            "probe seeded: scanned at epoch 7"
        );
    };
    seed_scan(&mut app);

    let fresh = app.config.path_feed_fingerprints();
    app.refresh_path_feeds(fresh);
    assert!(!app.sparkle_dirty, "nothing changed ⇒ no recompute re-arm");
    assert!(
        !app.windows[&wid].fx.decos().needs_rescan(7),
        "…and no word-deco reset (the settings-commit + watcher dedupe win)"
    );

    std::fs::write(&lex, "# lexicon v2\n").unwrap();
    let fresh = app.config.path_feed_fingerprints();
    app.refresh_path_feeds(fresh);
    assert!(
        app.sparkle_dirty,
        "lexicon content change re-arms the recompute"
    );
    assert!(
        app.windows[&wid].fx.decos().needs_rescan(7),
        "changed lexicon hard-resets the per-window decorations"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn deco_feed_gate_sees_lexicon_content_behind_an_unrelated_edit() {
    let old = cfg("font_px = 12.0\n");
    let new = cfg("font_px = 14.0\n");
    assert!(
        !super::deco_feed_changed(&old, &new, 7, 7),
        "an unrelated edit with unchanged feeds must not wipe decorations"
    );
    assert!(
        super::deco_feed_changed(&old, &new, 8, 7),
        "a lexicon-file edit behind an unrelated config edit must reset"
    );
    let sparkled = cfg("font_px = 12.0\n[sparkle_words]\nenabled = false\n");
    assert!(
        super::deco_feed_changed(&old, &sparkled, 7, 7),
        "a [sparkle_words] table edit resets as before"
    );
}
