// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::feed_paths_of;
use super::post_harness;
use super::{
    FailureLatch, POLL_INTERVAL, THEME_IDLE_POLL_INTERVAL, ThemePollSchedule, WatchFailure,
    WatchFailureKind, WatchStatusEvent, WatchStatusState, WatchTarget, coherent_theme_sample_with,
    config_failure_kind, config_file_observation, config_file_stamp, config_path_stamp,
    initial_config_observation_changed, observed_harness, path_feed_stamp,
    prepare_config_edge_with, prepare_theme_edge_with, theme_directory_stamp,
    theme_directory_stamp_capped,
};

/// ONE `stat` PER PATH THAT IS NOT A SYMLINK (unix): a regular file, a
/// folder and an absent path are hashed from their `lstat` alone, a
/// symlink and a dangling one are followed, and the stamp is the value the
/// two calls per path gave before, so the warm spare's learned gate, keyed
/// by a generation stamped with it and persisted, survives the update.
#[cfg(unix)]
#[test]
fn the_feed_stamp_follows_only_symlinks_and_keeps_its_value() {
    use std::hash::{Hash as _, Hasher as _};
    let dir = std::env::temp_dir().join(format!("aterm-feed-stamp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("folder")).unwrap();
    std::fs::write(dir.join("file"), b"feed").unwrap();
    std::os::unix::fs::symlink(dir.join("file"), dir.join("link")).unwrap();
    std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("dangling")).unwrap();
    let paths: Vec<std::path::PathBuf> = ["file", "folder", "link", "dangling", "absent"]
        .iter()
        .map(|name| dir.join(name))
        .collect();
    let mut follows = 0;
    let stamp = super::path_feed_stamp_with(&paths, |path| {
        follows += 1;
        std::fs::metadata(path)
    });
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "aterm-path-feed-metadata-v1".hash(&mut hasher);
    for path in &paths {
        path.hash(&mut hasher);
        match std::fs::symlink_metadata(path) {
            Ok(logical) => {
                super::hash_file_metadata(&logical, &mut hasher);
                if logical.file_type().is_symlink() {
                    std::fs::read_link(path).ok().hash(&mut hasher);
                }
            }
            Err(_) => "absent".hash(&mut hasher),
        }
        match std::fs::metadata(path) {
            Ok(followed) => super::hash_file_metadata(&followed, &mut hasher),
            Err(_) => "target-absent".hash(&mut hasher),
        }
    }
    let production = path_feed_stamp(&paths);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        follows, 2,
        "only the symlink and the dangling one are followed"
    );
    assert_eq!(stamp, hasher.finish(), "the stamp two calls per path gave");
    assert_eq!(production, stamp, "the production stamp is the same");
}

/// Return once the clock the filesystem stamps a ctime from has moved past
/// `path`'s ctime, so a rewrite now carries a ctime of its own.
#[cfg(unix)]
fn after_ctime_tick(path: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    let ctime = |p: &std::path::Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.ctime(), m.ctime_nsec())
    };
    let was = ctime(path);
    let probe = std::env::temp_dir().join(format!(
        "aterm-ctime-tick-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        std::fs::write(&probe, b"").unwrap();
        if ctime(&probe) > was {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the filesystem's clock did not move past a ctime in a minute"
        );
        aterm_uds::deadline::poll_yield();
    }
    let _ = std::fs::remove_file(&probe);
}

/// Editing only the lexicon hot-reloads it: the feeds the config names are stamped beside it,
/// so a same-length, mtime-restored lexicon rewrite under an unchanged `aterm.toml` moves the
/// feed stamp and re-posts the observation.
#[cfg(unix)]
#[test]
fn a_lexicon_edit_under_an_unchanged_config_is_a_reload_edge() {
    let dir =
        std::env::temp_dir().join(format!("aterm-config-watch-lexicon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("aterm.toml");
    let lexicon = dir.join("lexicon.toml");
    let toy = dir.join("toy.toml");
    std::fs::write(&lexicon, b"[[entry]]\nword = \"purr\"\n").unwrap();
    crate::test_scratch::write_config(
        &config,
        format!(
            "[sparkle_words]\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
            lexicon.display().to_string(),
            toy.display().to_string(),
        ),
    );
    let observation = config_file_observation(&config).unwrap();
    let feeds = feed_paths_of(&observation.text).expect("the config parses");
    assert_eq!(feeds, vec![lexicon.clone(), toy.clone()]);
    let config_stamp = config_path_stamp(&config).unwrap();
    let before = path_feed_stamp(&feeds);
    assert_eq!(path_feed_stamp(&feeds), before, "control: idle is no edge");

    let metadata = std::fs::metadata(&lexicon).unwrap();
    let modified = metadata.modified().unwrap();
    after_ctime_tick(&lexicon);
    std::fs::write(&lexicon, b"[[entry]]\nword = \"mews\"\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&lexicon)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(config_path_stamp(&config).unwrap(), config_stamp);
    let moved = path_feed_stamp(&feeds) != before;
    assert!(moved, "a lexicon rewrite moves the feed stamp");
    let mut observed = 0;
    let edge = prepare_config_edge_with(Some(&config_stamp), config_stamp.clone(), moved, || {
        observed += 1;
    });
    assert!(
        edge.is_some() && observed == 1,
        "…and re-observes the config"
    );

    let absent = path_feed_stamp(&feeds);
    std::fs::write(&toy, b"").unwrap();
    assert_ne!(path_feed_stamp(&feeds), absent);
    let _ = std::fs::remove_dir_all(dir);
}

/// A disabled Sparkle table names no feed (it has no consumer), and the
/// lists are capped exactly as their loaders cap them.
#[test]
fn path_feed_paths_follow_their_consumers() {
    let off = crate::app_config::Config::parse(
        "[sparkle_words]\nenabled = false\nlexicon = \"/l.toml\"\ntoy_packs = [\"/t.toml\"]\n",
    )
    .unwrap();
    assert!(off.path_feed_paths().is_empty());
    let on = crate::app_config::Config::parse(
        "[sparkle_words]\nlexicon = \"/l.toml\"\ntoy_packs = [\"/t.toml\"]\n",
    )
    .unwrap();
    assert_eq!(
        on.path_feed_paths(),
        vec![
            std::path::PathBuf::from("/l.toml"),
            std::path::PathBuf::from("/t.toml")
        ]
    );
    let many: Vec<String> = (0..12).map(|i| format!("\"/p{i}.toml\"")).collect();
    let packs = crate::app_config::Config::parse(&format!(
        "[sparkle_words]\ntoy_packs = [{}]\n",
        many.join(", ")
    ))
    .unwrap();
    assert_eq!(packs.path_feed_paths().len(), 8, "the loaders' cap");

    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-config-watch-ai-fleet-")
        .tempdir()
        .unwrap();
    let dir = scratch.path();
    let fleet = dir.join(aterm_agent::ai::config::FLEET_FILE);
    let read = || crate::app_config::Config {
        ai: crate::app_config::AiPolicy::read_beside("", Some(dir)),
        ..crate::app_config::Config::default()
    };
    let absent = read();
    assert!(matches!(
        absent.ai.settings().fleet,
        aterm_agent::ai::config::FleetState::None
    ));
    let feeds = absent.path_feed_paths();
    assert_eq!(feeds, vec![fleet.clone()]);
    let missing_stamp = path_feed_stamp(&feeds);
    assert_eq!(path_feed_stamp(&feeds), missing_stamp, "idle is no edge");

    crate::test_scratch::write_config(&fleet, "schema = 1\npreferred = \"codex\"\n");
    assert_ne!(
        path_feed_stamp(&feeds),
        missing_stamp,
        "creation is an edge"
    );
    let present = read();
    assert_eq!(present.ai.settings().preferred.0.id, "codex");
    let present_stamp = path_feed_stamp(&present.path_feed_paths());
    std::fs::remove_file(&fleet).unwrap();
    assert_ne!(
        path_feed_stamp(&feeds),
        present_stamp,
        "deletion is an edge"
    );
    let deleted = read();
    assert_eq!(
        deleted.path_feed_paths(),
        feeds,
        "absence retains the watch"
    );
    assert!(matches!(
        deleted.ai.settings().fleet,
        aterm_agent::ai::config::FleetState::None
    ));
    let explicit = crate::app_config::AiPolicy::read_beside(
        &format!("[ai]\nfleet = {:?}\n", fleet.display().to_string()),
        Some(dir),
    );
    assert!(
        matches!(
            explicit.settings().fleet,
            aterm_agent::ai::config::FleetState::Refused { .. }
        ),
        "an explicitly requested missing document is still refused"
    );
    let missing_stamp = path_feed_stamp(&deleted.path_feed_paths());
    crate::test_scratch::write_config(&fleet, "schema = 1\npreferred = \"agy\"\n");
    assert_ne!(
        path_feed_stamp(&feeds),
        missing_stamp,
        "recreation is an edge"
    );
    assert_eq!(read().ai.settings().preferred.0.id, "agy");
}

#[test]
fn idle_config_poll_reads_no_content_until_an_identity_edge_or_retry() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-idle-cost-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "theme = \"Default\"\n");
    let stamp = config_path_stamp(&path).unwrap();
    let mut content_loads = 0;

    let idle = prepare_config_edge_with(Some(&stamp), stamp.clone(), false, || {
        content_loads += 1;
    });
    assert!(idle.is_none());
    assert_eq!(content_loads, 0, "a healthy idle poll must not open TOML");

    let retry = prepare_config_edge_with(Some(&stamp), stamp.clone(), true, || {
        content_loads += 1;
    });
    assert!(
        retry.is_some(),
        "a failed exact observation must be retried"
    );
    assert_eq!(content_loads, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn config_metadata_edge_tracks_same_length_rewrite_with_restored_mtime() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-metadata-edge-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, b"theme = \"Nord\"\n");
    let metadata = std::fs::metadata(&path).unwrap();
    let modified = metadata.modified().unwrap();
    let accessed = metadata.accessed().unwrap();
    let before = config_path_stamp(&path).unwrap();

    after_ctime_tick(&path);
    crate::test_scratch::write_config(&path, b"theme = \"Rose\"\n");
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(modified)
            .set_accessed(accessed),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    assert_ne!(config_path_stamp(&path).unwrap(), before);
    let _ = std::fs::remove_dir_all(dir);
}

/// Tier-1 projection for `HarnessPolicyOneWriter`: the file on disk, the
/// watcher's dedupe (`post_harness`'s `last`), its post in flight, the
/// policy the window runs (`App::config.harness`), whether the window runs
/// what it handles yet, and the Commit's pending forget.
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_harness_writer(
    model: &aterm_spec::derive::Model,
    disk: i64,
    edits: i64,
    last: Option<&crate::app_config::HarnessPolicy>,
    queued: Option<&crate::app_config::HarnessPolicy>,
    in_force: &crate::app_config::HarnessPolicy,
    committed: bool,
    forget_pending: bool,
) -> aterm_spec::interp::State {
    let bit = |p: &crate::app_config::HarnessPolicy| i64::from(p.policy.enabled);
    let mut state = model.init_state();
    state.insert("disk", disk);
    state.insert("edits", edits);
    state.insert("has_posted", i64::from(last.is_some()));
    state.insert("last_posted", last.map_or(0, bit));
    state.insert("queued", i64::from(queued.is_some()));
    state.insert("queued_val", queued.map_or(0, bit));
    state.insert("in_force", bit(in_force));
    state.insert("committed", i64::from(committed));
    state.insert("forget_pending", i64::from(forget_pending));
    state
}

/// The real parts of the one-writer protocol, driven step by step.
struct HarnessWriterDrive {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
    app: crate::App,
    last: Option<crate::app_config::HarnessPolicy>,
    /// The watcher's one source of stamps.
    stamps: crate::harness_policy_cell::HarnessStampSource,
    /// The watcher's post in flight in the event loop's queue.
    queued: Option<crate::harness_policy_cell::StampedHarnessPolicy>,
    committed: bool,
    /// The Commit's request to the watcher: raised, its forget is pending.
    repost: super::Repost,
    disk: i64,
    edits: i64,
    model: aterm_spec::derive::Model,
}

impl HarnessWriterDrive {
    fn state(&self) -> aterm_spec::interp::State {
        project_harness_writer(
            &self.model,
            self.disk,
            self.edits,
            self.last.as_ref(),
            self.queued.as_ref().map(|stamped| stamped.policy()),
            self.app.harness_policy.get(),
            self.committed,
            self.repost
                .raised
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Run `act` and validate the real transition against `action` (both
    /// tiers); a transition that moves nothing is a stutter.
    fn step(&mut self, action: &str, act: impl FnOnce(&mut Self)) {
        let before = self.state();
        act(self);
        let after = self.state();
        if before == after {
            return;
        }
        let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            &before,
            &after,
            Some(action),
            action,
        );
        assert!(conforms, "{action}: {before:?} -> {after:?}: {evidence}");
        for invariant in &self.model.invariants {
            assert!(
                self.model.check_invariant(invariant.name, &after),
                "{action}: {} fails in {after:?}",
                invariant.name
            );
        }
    }

    /// The file changes on disk: the kill switch goes in refused (a stray
    /// non-UTF-8 byte), approvals go in readable.
    fn write(&mut self, approve: bool) {
        let action = if approve { "WriteOn" } else { "WriteOff" };
        self.step(action, |d| {
            if approve {
                crate::test_scratch::write_config(
                    &d.path,
                    "font_px = 14\n[harness]\napprove = \"all\"\n",
                );
            } else {
                crate::test_scratch::write_config(
                    &d.path,
                    b"# caf\xe9\n[harness]\nenabled = false\n",
                );
            }
            d.disk = i64::from(approve);
            d.edits += 1;
        });
    }

    /// One watcher read of the file and its post: the real
    /// `observed_harness` and `post_harness`, whose send queues the wake.
    fn poll(&mut self) {
        if self.queued.is_some() {
            return;
        }
        self.step("Poll", |d| {
            let read = config_file_observation(&d.path);
            let policy = observed_harness(Some(&d.path), &read).expect("a policy per read");
            let queue = &mut d.queued;
            assert!(post_harness(
                |stamped| {
                    *queue = Some(stamped);
                    true
                },
                &mut d.last,
                &mut d.stamps,
                policy,
            ));
        });
    }

    /// The window handles the queued post: the real handler
    /// (`adopt_harness_policy`) once it runs what it handles; dropped
    /// before a seamless successor's Commit.
    fn deliver(&mut self) {
        self.step("Deliver", |d| {
            if let Some(policy) = d.queued.take()
                && d.committed
            {
                d.app.adopt_harness_policy(policy);
            }
        });
    }

    /// The config lane admits a generation carrying its own parse — the
    /// real `apply_prepared_config_generation`, fed a stale file.
    fn admit(&mut self, approve: bool) {
        let action = if approve { "AdmitOn" } else { "AdmitOff" };
        self.step(action, |d| {
            let text = if approve {
                "font_px = 16\n[harness]\napprove = \"all\"\n"
            } else {
                "font_px = 12\n[harness]\nenabled = false\n"
            };
            let staged = d.dir.join("admitted.toml");
            crate::test_scratch::write_config(&staged, text);
            let observation =
                crate::native_config_service::VersionedConfigService::observe_path(&staged, false)
                    .unwrap();
            let config = crate::app_config::Config::parse(text).unwrap();
            assert_eq!(
                config.harness.policy.enabled, approve,
                "control: its own parse"
            );
            let assets = config.resolve_asset_catalog();
            let feeds = config.path_feed_fingerprints();
            let sparkle = config.prepare_sparkle_runtime();
            d.app.apply_prepared_config_generation(
                crate::native_font_catalog::PreparedConfigGeneration {
                    observation,
                    config,
                    values: std::collections::BTreeMap::new(),
                    disk_faults: std::sync::Arc::default(),
                    assets,
                    path_feed_fps: feeds,
                    sparkle,
                    fonts: None,
                    warnings: Vec::new(),
                    font_refusals: Vec::new(),
                    reason: crate::app_colorscheme::ThemeChange::ConfigReload,
                },
            );
        });
    }

    /// A seamless successor's Commit: the window runs what it handles from
    /// here on, and the handler raises the watcher's `Repost`.
    fn commit(&mut self) {
        self.step("Commit", |d| {
            d.committed = true;
            d.repost.raise();
        });
    }

    /// The watcher's next step takes the request and forgets what it posted: the real
    /// `forget_after_commit`.
    fn forget(&mut self) {
        self.step("Forget", |d| {
            super::forget_after_commit(&d.repost, &mut d.last);
        });
    }
}

/// Tier-1 for `HarnessPolicyOneWriter`: drives the real read (`observed_harness`), post and
/// dedupe (`post_harness`), the window's handler and the admission path through writes of a
/// refused kill switch and a readable approval, polls around a seamless Commit, and stale
/// admissions of both policies at the worst moments, validating every transition and
/// invariant against the derived model.
#[test]
fn the_watcher_is_the_policys_one_writer() {
    let dir = std::env::temp_dir().join(format!("aterm-harness-one-writer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, b"# caf\xe9\n[harness]\nenabled = false\n");
    let app = crate::App::headless_for_test();
    assert!(
        app.harness_policy.get().policy.enabled,
        "the launch ran approvals on"
    );
    let mut drive = HarnessWriterDrive {
        dir: dir.clone(),
        path,
        app,
        last: None,
        stamps: crate::harness_policy_cell::HarnessStampSource::new(),
        queued: None,
        committed: false,
        repost: super::Repost::new(),
        disk: 0,
        edits: 0,
        model: aterm_models_harness::harness_policy_one_writer_model(),
    };
    assert_eq!(drive.state(), drive.model.init_state(), "the model's start");

    drive.poll();
    drive.deliver();
    drive.commit();
    drive.poll();
    drive.forget();
    drive.poll();
    drive.admit(true);
    drive.deliver();
    assert!(
        !drive.app.harness_policy.get().policy.enabled,
        "the kill switch runs"
    );
    drive.admit(true);
    assert!(
        !drive.app.harness_policy.get().policy.enabled,
        "it still runs"
    );
    drive.poll();
    drive.write(true);
    drive.admit(false);
    assert!(
        !drive.app.harness_policy.get().policy.enabled,
        "only the watcher's read moves the policy"
    );
    drive.poll();
    drive.deliver();
    assert!(
        drive.app.harness_policy.get().policy.enabled,
        "the approval runs"
    );
    drive.write(false);
    drive.poll();
    drive.write(true);
    drive.admit(true);
    drive.deliver();
    assert!(!drive.app.harness_policy.get().policy.enabled);
    drive.poll();
    drive.deliver();
    assert!(
        drive.app.harness_policy.get().policy.enabled,
        "the newest read runs"
    );

    let before = drive.state();
    let mut mutant = before.clone();
    mutant.insert("in_force", 1 - before["in_force"]);
    let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
        &drive.model,
        &[],
        &before,
        &mutant,
        Some(if before["in_force"] == 1 {
            "AdmitOff"
        } else {
            "AdmitOn"
        }),
        "second writer",
    );
    assert!(!conforms, "a second writer conformed: {evidence}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A chmod alone must withdraw the policy, without waiting for a content edit.
#[test]
#[cfg(unix)]
fn a_metadata_only_config_refusal_withdraws_the_live_harness_policy() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("aterm.toml");
    let text = "[harness]\nenabled = true\napprove = \"all\"\n";
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = config_path_stamp(&path).unwrap();
    let admitted = config_file_observation(&path).unwrap();
    let mut last = None;
    let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
    let mut posted = Vec::new();
    let policy = observed_harness(Some(&path), &Ok(admitted)).unwrap();
    assert!(policy.policy.enabled);
    assert!(post_harness(
        |policy| {
            posted.push(policy);
            true
        },
        &mut last,
        &mut stamps,
        policy
    ));

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    let after = config_path_stamp(&path).unwrap();
    assert_ne!(before, after, "permissions alone are a metadata edge");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let (_, refused) = prepare_config_edge_with(Some(&before), after, false, || {
        config_file_observation(&path)
    })
    .unwrap();
    assert!(refused.is_err());
    let off = observed_harness(Some(&path), &refused)
        .expect("an ownership refusal must replace prior harness authority");
    assert!(!off.policy.enabled);
    assert!(post_harness(
        |policy| {
            posted.push(policy);
            true
        },
        &mut last,
        &mut stamps,
        off.clone()
    ));
    assert!(!last.as_ref().unwrap().policy.enabled);
    assert_eq!(posted.len(), 2);
    assert!(post_harness(
        |policy| {
            posted.push(policy);
            true
        },
        &mut last,
        &mut stamps,
        off
    ));
    assert_eq!(posted.len(), 2, "an unchanged refusal does not repost");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let restored = observed_harness(Some(&path), &config_file_observation(&path)).unwrap();
    assert!(restored.policy.enabled);
    assert!(post_harness(
        |policy| {
            posted.push(policy);
            true
        },
        &mut last,
        &mut stamps,
        restored
    ));
    assert_eq!(posted.len(), 3);
}

/// A REFUSED READ REACHES THE WINDOW: a file this watcher's admission refuses never posts an
/// observation, so a launch's off outlasted the fault or the line behind it for the process's
/// life, and a line added to a file a window had read well was never heard. Each new text goes
/// once, and a Commit's forgetting sends it again.
#[test]
fn a_refused_read_reaches_the_window() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("aterm.toml");
    let posts = |reads: &mut super::RefusedReads| {
        let mut sent = Vec::new();
        assert!(super::post_refused_update_text(reads, &path, |text| {
            sent.push(text);
            true
        }));
        sent
    };
    let on_disk = || {
        aterm_update_core::settings::read_config_text(&path)
            .expect("read")
            .expect("a file")
    };
    std::fs::write(&path, b"# caf\xe9\n[update]\n").expect("write");
    assert!(
        super::config_file_observation(&path).is_err(),
        "PRECONDITION: the watcher's admission refuses this file"
    );
    let says_nothing = on_disk();
    assert!(!says_nothing.contains("auto_apply"));
    let mut reads = super::RefusedReads::default();
    assert!(posts(&mut reads).is_empty(), "one read is no text");
    assert_eq!(posts(&mut reads), [says_nothing]);
    assert!(posts(&mut reads).is_empty(), "each text goes once");
    std::fs::write(&path, b"# caf\xe9\n[update]\nauto_apply = false\n").expect("write");
    let says_off = on_disk();
    assert!(posts(&mut reads).is_empty(), "one read is no text");
    assert_eq!(
        posts(&mut reads),
        std::slice::from_ref(&says_off),
        "a line added to a file that stays refused is seen"
    );
    reads.sent = None;
    assert_eq!(
        posts(&mut reads),
        [says_off],
        "a Commit's forgetting sends it again"
    );
    let mut gone = super::RefusedReads::default();
    assert!(super::post_refused_update_text(&mut gone, &path, |_| false));
    assert!(
        !super::post_refused_update_text(&mut gone, &path, |_| false),
        "the window is gone"
    );
    std::fs::remove_file(&path).expect("remove");
    assert!(posts(&mut reads).is_empty());
    assert_eq!(posts(&mut reads), [String::new()], "no file reads as on");
}

/// A SAVE CAUGHT HALF WAY REACHES NO WINDOW: a read of a prefix lifted a person's `false`, and a
/// fault that can pass right after it kept that going for as long as the fault lasted. CONTROLS:
/// a save that stalls across two polls does reach it, and a whole text goes after the fault.
#[test]
fn a_save_caught_half_way_reaches_no_window() {
    use std::io::{Error, ErrorKind};
    const WHOLE: &str = "[update]\nauto_apply = false\n";
    const TORN: &str = "[update]\n";
    let mut reads = super::RefusedReads::default();
    let mut posts = |read: std::io::Result<Option<String>>| {
        let mut sent = Vec::new();
        assert!(super::post_refused_read(&mut reads, read, |text| {
            sent.push(text);
            true
        }));
        sent
    };
    let text = |text: &str| Ok(Some(text.to_owned()));
    let fault = || Err(Error::from(ErrorKind::TimedOut));
    assert!(posts(text(WHOLE)).is_empty());
    assert_eq!(posts(text(WHOLE)), [WHOLE]);
    assert!(posts(text(TORN)).is_empty(), "one read of a torn save");
    assert!(posts(fault()).is_empty(), "and a fault right after it");
    assert!(
        posts(text(TORN)).is_empty(),
        "the fault started the agreement again"
    );
    assert!(posts(text(WHOLE)).is_empty(), "the save finished");
    assert!(
        posts(text(WHOLE)).is_empty(),
        "the window already holds this text"
    );
    assert!(posts(text(TORN)).is_empty());
    assert_eq!(
        posts(text(TORN)),
        [TORN],
        "CONTROL: a save that stalls across two polls reads as what it is"
    );
    assert!(posts(fault()).is_empty());
    assert!(posts(text(WHOLE)).is_empty());
    assert_eq!(
        posts(text(WHOLE)),
        [WHOLE],
        "CONTROL: the whole text goes after the fault"
    );
}

#[test]
fn a_failed_stamp_replaces_prior_policy_and_missing_context_cannot_enable_it() {
    let mut last = Some(crate::app_config::HarnessPolicy::read(
        "[harness]\nenabled = true\n",
    ));
    let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
    let mut posted = Vec::new();
    assert!(last.as_ref().unwrap().policy.enabled);
    let path = std::path::Path::new("unavailable/aterm.toml");
    assert!(super::post_stamp_refusal(
        |policy| {
            posted.push(policy);
            true
        },
        &mut last,
        &mut stamps,
        path
    ));
    assert!(!last.as_ref().unwrap().policy.enabled);
    assert_eq!(posted.len(), 1);
    assert!(super::post_stamp_refusal(
        |policy| {
            posted.push(policy);
            true
        },
        &mut last,
        &mut stamps,
        path
    ));
    assert_eq!(posted.len(), 1, "an unchanged failed stamp does not repost");
    for error in [
        crate::native_document_host::DocumentHostError::InvalidUtf8,
        crate::native_document_host::DocumentHostError::TooLarge { limit: 1 },
    ] {
        assert!(!observed_harness(None, &Err(error)).unwrap().policy.enabled);
    }
    assert!(
        crate::app_config::HarnessPolicy::read_path(None)
            .policy
            .enabled,
        "explicit no-config startup retains its defaults"
    );
}

/// Accepted document observations keep their own text. Failed file observations use
/// the common reader, including owned lossy comments, its cap and missing-file defaults.
#[test]
fn every_watcher_read_carries_its_harness_policy() {
    use crate::native_document_host::DocumentHostError;
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-observed-harness-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");

    let text = "font_px = 13\n[harness]\napprove = \"safe\"\nenabled = false\n";
    crate::test_scratch::write_config(&path, text);
    let readable = config_file_observation(&path);
    let policy = observed_harness(Some(&path), &readable).expect("a readable read");
    assert_eq!(
        policy,
        crate::app_config::Config::parse(text).unwrap().harness,
        "the watcher's policy is the one the config's own parse reads"
    );
    assert!(!policy.policy.enabled, "{policy:?}");

    crate::test_scratch::write_config(&path, b"# caf\xe9\n[harness]\nenabled = false\n");
    let refused = config_file_observation(&path);
    assert!(
        matches!(refused, Err(DocumentHostError::InvalidUtf8)),
        "{refused:?}"
    );
    let policy = observed_harness(Some(&path), &refused).expect("a refused read");
    assert!(!policy.policy.enabled, "the kill switch holds: {policy:?}");

    crate::test_scratch::write_config(&path, b"# caf\xe9\n[harness]\napprove = \"all\"\n");
    let refused = config_file_observation(&path);
    let lifted = observed_harness(Some(&path), &refused).expect("a refused read");
    assert!(lifted.policy.enabled, "lifting it holds too: {lifted:?}");

    let big = format!(
        "[harness]\nenabled = false\n# {}\n",
        "x".repeat(crate::native_config_service::MAX_CONFIG_FILE_BYTES)
    );
    crate::test_scratch::write_config(&path, big);
    let refused = config_file_observation(&path);
    assert!(
        matches!(refused, Err(DocumentHostError::TooLarge { .. })),
        "{refused:?}"
    );
    let policy = observed_harness(Some(&path), &refused).expect("a refused read");
    assert!(!policy.policy.enabled, "{policy:?}");

    std::fs::remove_file(&path).unwrap();
    assert!(
        observed_harness(Some(&path), &refused)
            .unwrap()
            .policy
            .enabled,
        "a real missing file uses the absent-file defaults"
    );
    assert!(
        !observed_harness(Some(&path), &Err(DocumentHostError::NotAFile))
            .unwrap()
            .policy
            .enabled,
        "another refusal withdraws prior authority"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn startup_handoff_publishes_only_a_generation_newer_than_the_loaded_baseline() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-startup-handoff-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "font_px = 12\n");
    let startup = config_file_observation(&path).unwrap();
    assert!(!initial_config_observation_changed(
        Some(&startup.baseline),
        &startup
    ));

    crate::test_scratch::write_config(&path, "font_px = 14\n");
    let edited = config_file_observation(&path).unwrap();
    assert!(initial_config_observation_changed(
        Some(&startup.baseline),
        &edited
    ));
    assert!(
        initial_config_observation_changed(None, &edited),
        "a startup parse/read fallback has no authority to suppress a readable generation"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn config_stamp_tracks_same_length_content_when_mtime_is_restored() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-preserved-mtime-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, b"theme = \"Nord\"\n");
    let metadata = std::fs::metadata(&path).unwrap();
    let modified = metadata.modified().unwrap();
    let accessed = metadata.accessed().unwrap();
    let before = config_file_stamp(&path).unwrap();

    crate::test_scratch::write_config(&path, b"theme = \"Rose\"\n");
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(modified)
            .set_accessed(accessed),
    )
    .unwrap();
    let after_metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(after_metadata.len(), metadata.len());
    assert_eq!(after_metadata.modified().unwrap(), modified);
    assert_ne!(config_file_stamp(&path).unwrap(), before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn exact_observation_survives_transient_missing_then_restored_b() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-restored-b-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "font_px = 12\n");
    let a = config_file_observation(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let missing = config_file_observation(&path).unwrap();
    crate::test_scratch::write_config(&path, "font_px = 14\n");
    let b = config_file_observation(&path).unwrap();
    assert_ne!(a.baseline, missing.baseline);
    assert_ne!(missing.baseline, b.baseline);
    assert_eq!(b.text, "font_px = 14\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn config_stamp_uses_the_shared_runtime_and_manual_admission_cap() {
    let dir = std::env::temp_dir().join(format!("aterm-config-watch-limit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(crate::native_config_service::MAX_CONFIG_FILE_BYTES as u64 + 1)
        .unwrap();
    assert_eq!(
        config_file_stamp(&path),
        Err(crate::native_document_host::DocumentHostError::TooLarge {
            limit: crate::native_config_service::MAX_CONFIG_FILE_BYTES,
        })
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn config_stamp_tracks_symlink_retarget_with_identical_content() {
    use std::os::unix::fs::symlink;

    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-retarget-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("first.toml");
    let second = dir.join("second.toml");
    let logical = dir.join("aterm.toml");
    crate::test_scratch::write_config(&first, b"theme = \"Nord\"\n");
    crate::test_scratch::write_config(&second, b"theme = \"Nord\"\n");
    symlink(&first, &logical).unwrap();
    let before = config_file_stamp(&logical).unwrap();

    std::fs::remove_file(&logical).unwrap();
    symlink(&second, &logical).unwrap();
    let after = config_file_stamp(&logical).unwrap();
    assert_eq!(before.observed.content, after.observed.content);
    assert_ne!(after, before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_stamp_tracks_regular_conf_content_generation_and_membership() {
    let dir = std::env::temp_dir().join(format!("aterm-theme-watch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let empty = theme_directory_stamp(&dir).unwrap();
    assert_eq!(empty.candidate_metadata, 0);
    let theme = dir.join("Work.conf");
    std::fs::write(&theme, "foreground = #fff\n").unwrap();
    let created = theme_directory_stamp(&dir).unwrap();
    assert_ne!(created, empty);
    assert_eq!(created.candidate_metadata, 1);
    std::fs::write(&theme, "foreground = #ffffff\nbackground = #000000\n").unwrap();
    let edited = theme_directory_stamp(&dir).unwrap();
    assert_ne!(edited, created);
    std::fs::remove_file(theme).unwrap();
    let deleted = theme_directory_stamp(&dir).unwrap();
    assert_ne!(deleted, edited);
    assert_eq!(deleted.candidate_metadata, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn theme_stamp_tracks_same_length_content_when_mtime_is_restored() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-theme-watch-preserved-mtime-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let theme = dir.join("Work.conf");
    std::fs::write(&theme, b"foreground = #112233\n").unwrap();
    let metadata = std::fs::metadata(&theme).unwrap();
    let modified = metadata.modified().unwrap();
    let accessed = metadata.accessed().unwrap();
    let before = theme_directory_stamp(&dir).unwrap();

    after_ctime_tick(&theme);
    std::fs::write(&theme, b"foreground = #445566\n").unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&theme)
        .unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(modified)
            .set_accessed(accessed),
    )
    .unwrap();
    let after_metadata = std::fs::metadata(&theme).unwrap();
    assert_eq!(after_metadata.len(), metadata.len());
    assert_eq!(after_metadata.modified().unwrap(), modified);
    assert_ne!(theme_directory_stamp(&dir).unwrap(), before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_sample_retries_mutation_between_stamp_and_discovery() {
    let dir =
        std::env::temp_dir().join(format!("aterm-theme-watch-coherent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let theme = dir.join("Work.conf");
    std::fs::write(&theme, "foreground = #111111\n").unwrap();
    let mut calls = 0;
    let initial = theme_directory_stamp(&dir).unwrap();
    let (_, catalog) = coherent_theme_sample_with(&dir, initial, || {
        calls += 1;
        let catalog = std::sync::Arc::new(crate::app_config::ThemeCatalog::discover_in(&dir));
        if calls == 1 {
            #[cfg(unix)]
            after_ctime_tick(&theme);
            std::fs::write(&theme, "foreground = #222222\n").unwrap();
        }
        Ok(catalog)
    })
    .expect("second sample is stable");
    assert!(calls >= 2, "the split generation must be retried");
    let expected = crate::app_config::ThemeCatalog::discover_in(&dir);
    assert_eq!(*catalog, expected);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_stamp_bounds_total_entries_and_detects_return_under_cap() {
    const CAP: usize = 8;
    let caps = crate::app_config::ThemeScanCaps {
        entries: CAP,
        ..crate::app_config::ThemeScanCaps::PRODUCTION
    };
    let dir = std::env::temp_dir().join(format!(
        "aterm-theme-watch-entry-cap-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let noise = (0..=CAP)
        .map(|index| dir.join(format!("noise-{index:04}")))
        .collect::<Vec<_>>();
    for path in &noise {
        std::fs::write(path, []).unwrap();
    }
    let over = theme_directory_stamp_capped(&dir, caps).unwrap();
    assert!(over.truncated);
    assert_eq!(over.inspected_entries, CAP + 1);
    std::fs::remove_file(&noise[0]).unwrap();
    let under = theme_directory_stamp_capped(&dir, caps).unwrap();
    assert_ne!(over, under, "returning under the entry cap is observable");
    assert!(!under.truncated);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn idle_theme_poll_reads_no_theme_contents_and_bounds_metadata() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-theme-watch-idle-cost-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let caps = crate::app_config::ThemeScanCaps {
        candidates: 4,
        ..crate::app_config::ThemeScanCaps::PRODUCTION
    };
    let cap = caps.candidates;
    for index in 0..cap + 8 {
        std::fs::write(
            dir.join(format!("Theme-{index:04}.conf")),
            "foreground = #112233\n",
        )
        .unwrap();
    }
    let stamp = theme_directory_stamp_capped(&dir, caps).unwrap();
    assert_eq!(stamp.inspected_entries, cap + 8);
    assert_eq!(stamp.candidate_metadata, cap);

    let mut content_loads = 0;
    let result = prepare_theme_edge_with(&dir, Some(&stamp), stamp.clone(), || {
        content_loads += 1;
        Ok(crate::app_config::ThemeCatalog::empty())
    });
    assert!(result.is_none());
    assert_eq!(content_loads, 0, "an idle poll must not open theme files");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_scans_back_off_only_after_a_stable_sample_and_follow_edges_quickly() {
    use std::time::Instant;

    let dir = std::env::temp_dir().join(format!(
        "aterm-theme-watch-idle-cadence-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let start = Instant::now();
    let empty = theme_directory_stamp(&dir).unwrap();
    let mut schedule = ThemePollSchedule::new(start, Some(Ok(empty.clone())));

    assert!(!schedule.due(start + POLL_INTERVAL - std::time::Duration::from_nanos(1)));
    let first = start + POLL_INTERVAL;
    assert!(schedule.due(first));
    schedule.observed(first, Ok(empty));
    assert!(!schedule.due(first + POLL_INTERVAL));
    let idle_due = first + THEME_IDLE_POLL_INTERVAL;
    assert!(schedule.due(idle_due));

    std::fs::write(dir.join("Work.conf"), "foreground = #112233\n").unwrap();
    let created = theme_directory_stamp(&dir).unwrap();
    schedule.observed(idle_due, Ok(created.clone()));
    assert!(!schedule.due(idle_due + POLL_INTERVAL - std::time::Duration::from_nanos(1)));
    let follow_up = idle_due + POLL_INTERVAL;
    assert!(schedule.due(follow_up));
    schedule.observed(follow_up, Ok(created));
    assert!(!schedule.due(follow_up + POLL_INTERVAL));
    assert!(schedule.due(follow_up + THEME_IDLE_POLL_INTERVAL));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_failure_and_recovery_get_one_fast_follow_up_each() {
    use std::time::Instant;

    let start = Instant::now();
    let mut schedule = ThemePollSchedule::new(start, None);
    let failed_at = start + POLL_INTERVAL;
    schedule.observed(failed_at, Err(WatchFailureKind::ThemeDirectoryUnreadable));
    let repeated_at = failed_at + POLL_INTERVAL;
    assert!(schedule.due(repeated_at));
    schedule.observed(repeated_at, Err(WatchFailureKind::ThemeDirectoryUnreadable));
    assert!(!schedule.due(repeated_at + POLL_INTERVAL));
    let recovered_at = repeated_at + THEME_IDLE_POLL_INTERVAL;
    assert!(schedule.due(recovered_at));
    let recovered = super::ThemeDirectoryStamp {
        fingerprint: 1,
        inspected_entries: 0,
        candidate_metadata: 0,
        truncated: false,
    };
    schedule.observed(recovered_at, Ok(recovered));
    assert!(schedule.due(recovered_at + POLL_INTERVAL));
}

#[test]
fn theme_directory_errors_are_not_empty_catalog_generations() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-theme-watch-read-error-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let not_a_directory = dir.join("themes.conf");
    std::fs::write(&not_a_directory, "foreground = #112233\n").unwrap();
    assert_eq!(
        theme_directory_stamp(&not_a_directory),
        Err(WatchFailureKind::ThemeDirectoryUnreadable)
    );
    assert!(
        crate::app_config::ThemeCatalog::try_discover_in(&not_a_directory).is_err(),
        "a read_dir failure must not manufacture an empty catalog"
    );

    let missing = dir.join("deleted");
    let stamp = theme_directory_stamp(&missing).expect("NotFound is a valid empty generation");
    assert_eq!(stamp.candidate_metadata, 0);
    let empty = crate::app_config::ThemeCatalog::try_discover_in(&missing)
        .expect("NotFound maps to an empty catalog");
    assert_eq!(empty, *crate::app_config::ThemeCatalog::empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn failure_edges_are_deduplicated_and_recovery_clears_only_its_target() {
    let mut latch = FailureLatch::default();
    let failure = WatchFailure {
        target: WatchTarget::Config,
        kind: WatchFailureKind::ConfigInvalidUtf8,
    };
    assert_eq!(
        latch.transition(
            WatchTarget::Config,
            Some(WatchFailureKind::ConfigInvalidUtf8)
        ),
        Some(WatchStatusEvent::Failed(failure))
    );
    assert_eq!(
        latch.transition(
            WatchTarget::Config,
            Some(WatchFailureKind::ConfigInvalidUtf8)
        ),
        None,
        "a stable failure posts no repeated wake"
    );
    assert_eq!(
        latch.transition(WatchTarget::Config, None),
        Some(WatchStatusEvent::Recovered(WatchTarget::Config))
    );
    assert_eq!(latch.transition(WatchTarget::Config, None), None);

    let mut state = WatchStatusState::default();
    assert!(state.reduce(WatchStatusEvent::Failed(failure)));
    assert!(state.reduce(WatchStatusEvent::Failed(WatchFailure {
        target: WatchTarget::Themes,
        kind: WatchFailureKind::ThemeDirectoryUnreadable,
    })));
    assert!(state.reduce(WatchStatusEvent::Recovered(WatchTarget::Config)));
    let message = state.message().expect("theme failure remains visible");
    assert!(!message.contains("aterm.toml"));
    assert!(message.contains("Custom themes"));
    assert!(state.reduce(WatchStatusEvent::Recovered(WatchTarget::Themes)));
    assert_eq!(state.message(), None);
}

#[test]
fn watcher_failure_latch_conforms_to_the_derived_protocol() {
    let model = aterm_models_native::watcher_failure_recovery_model();
    let mut modeled = model.init_state();
    let mut latch = FailureLatch::default();
    let mut failure_wakes = 0_i64;
    let mut recovery_wakes = 0_i64;

    let failed = latch.transition(
        WatchTarget::Themes,
        Some(WatchFailureKind::ThemeDirectoryUnreadable),
    );
    failure_wakes += i64::from(matches!(failed, Some(WatchStatusEvent::Failed(_))));
    modeled = model.successors("ObserveFailure", &modeled)[0].clone();
    assert_eq!(i64::from(latch.kind.is_some()), modeled["failed"]);
    assert_eq!(failure_wakes, modeled["failure_wakes"]);

    let repeated = latch.transition(
        WatchTarget::Themes,
        Some(WatchFailureKind::ThemeDirectoryUnreadable),
    );
    failure_wakes += i64::from(matches!(repeated, Some(WatchStatusEvent::Failed(_))));
    modeled = model.successors("RepeatFailure", &modeled)[0].clone();
    assert_eq!(repeated, None);
    assert_eq!(failure_wakes, modeled["failure_wakes"]);
    assert!(model.check_invariant("FailureWakeDeduped", &modeled));

    let recovered = latch.transition(WatchTarget::Themes, None);
    recovery_wakes += i64::from(matches!(recovered, Some(WatchStatusEvent::Recovered(_))));
    modeled = model.successors("Recover", &modeled)[0].clone();
    assert_eq!(i64::from(latch.kind.is_some()), modeled["failed"]);
    assert_eq!(recovery_wakes, modeled["recovery_wakes"]);
    assert!(model.check_invariant("FailureStatusExact", &modeled));

    let mut duplicate = modeled;
    duplicate.insert("failure_wakes", duplicate["failure_wakes"] + 1);
    assert!(
        !model.check_invariant("FailureWakeDeduped", &duplicate),
        "negative control: a duplicate wake must violate the model"
    );
}

#[test]
fn invalid_utf8_is_a_typed_config_rejection() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-invalid-utf8-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, [0xff, 0xfe]);
    let error = config_file_observation(&path).unwrap_err();
    assert_eq!(
        config_failure_kind(&error),
        WatchFailureKind::ConfigInvalidUtf8
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn only_the_latest_admitted_config_candidate_can_clear_failure_status() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-watch-admission-ticket-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "font_px = 12\n");
    let first = config_file_observation(&path).unwrap().baseline;
    crate::test_scratch::write_config(&path, "font_px = 14\n");
    let second = config_file_observation(&path).unwrap().baseline;
    assert_ne!(first, second);

    let mut state = WatchStatusState::default();
    let model = aterm_models_native::watcher_failure_recovery_model();
    let mut modeled = model.successors("ObserveFailure", &model.init_state())[0].clone();
    assert!(state.reduce(WatchStatusEvent::Failed(WatchFailure {
        target: WatchTarget::Config,
        kind: WatchFailureKind::ConfigInvalidToml,
    })));
    state.note_config_candidate(first.clone());
    modeled = model.successors("ObserveCandidateOne", &modeled)[0].clone();
    state.note_config_candidate(second.clone());
    modeled = model.successors("ObserveCandidateTwo", &modeled)[0].clone();
    assert!(
        !state.reject_config_candidate(&first, WatchFailureKind::ConfigPreparationFailed),
        "a stale rejection cannot invalidate the newer recovery ticket"
    );
    assert!(
        !state.acknowledge_config_candidate(&first),
        "an older async completion cannot clear a newer candidate"
    );
    assert!(state.message().is_some());
    modeled = model.successors("AdmitCandidateOne", &modeled)[0].clone();
    assert_eq!(modeled["failed"], 1);
    assert!(model.check_invariant("ConfigRecoveryAdmitsLatest", &modeled));
    assert_eq!(state.config_candidate.as_ref(), Some(&second));
    assert!(state.acknowledge_config_candidate(&second));
    modeled = model.successors("AdmitCandidateTwo", &modeled)[0].clone();
    assert_eq!(modeled["failed"], 0);
    assert_eq!(state.message(), None);

    state.note_config_candidate(second.clone());
    assert!(state.reduce(WatchStatusEvent::Failed(WatchFailure {
        target: WatchTarget::Config,
        kind: WatchFailureKind::ConfigUnreadable,
    })));
    assert!(
        !state.acknowledge_config_candidate(&second),
        "a later read failure invalidates the pending recovery ticket"
    );
    assert!(state.message().is_some());
    let _ = std::fs::remove_dir_all(dir);
}

/// Rewrite the config at `path` as an editor would, once the filesystem's clock has moved
/// past its last change: two same-length writes inside one tick leave every stamp equal.
fn rewrite(path: &std::path::Path, text: impl AsRef<[u8]>) {
    #[cfg(unix)]
    after_ctime_tick(path);
    crate::test_scratch::write_config(path, text);
}

/// The real watcher loop ([`super::watch`]) over scripted ears: a clock that moves only when
/// a rest runs out, every key held and let go (a key is never used twice), each rest's
/// deadline from the clock, and the script's turns, one at each rest. A turn may change
/// files and returns what it tells; one that tells nothing lets the rest run out. The watch
/// ends at the script's end.
struct Script {
    now: std::cell::Cell<std::time::Instant>,
    /// How long each look takes: every [`super::Ears::now`] the watcher asks is a step's end.
    taken: std::time::Duration,
    deaf: bool,
    held: std::collections::BTreeMap<u64, super::Hold>,
    keys: std::collections::BTreeSet<u64>,
    let_go: Vec<u64>,
    rests: Vec<Option<std::time::Duration>>,
    turns: std::collections::VecDeque<Turn>,
}

type Turn = Box<dyn FnOnce(&mut Script) -> Vec<super::Tell>>;

impl Script {
    fn new(deaf: bool, turns: Vec<Turn>) -> Self {
        Self {
            now: std::cell::Cell::new(std::time::Instant::now()),
            taken: std::time::Duration::ZERO,
            deaf,
            held: std::collections::BTreeMap::new(),
            keys: std::collections::BTreeSet::new(),
            let_go: Vec::new(),
            rests: Vec::new(),
            turns: turns.into(),
        }
    }

    /// A tell of each key held on `path`.
    fn on(&self, path: &std::path::Path) -> Vec<super::Tell> {
        let tells: Vec<_> = self
            .held
            .iter()
            .filter(|(_, hold)| {
                matches!(hold, super::Hold::File(held) | super::Hold::Directory(held) if held == path)
            })
            .map(|(key, _)| super::Tell::Changed(*key))
            .collect();
        assert!(!tells.is_empty(), "{} is held", path.display());
        tells
    }

    fn rest(&self) -> Option<std::time::Duration> {
        *self.rests.last().expect("a rest")
    }

    fn holds(&self) -> std::collections::BTreeSet<super::Hold> {
        self.held.values().cloned().collect()
    }
}

impl super::Ears for Script {
    fn watch(&mut self, key: u64, path: std::path::PathBuf) {
        assert!(self.keys.insert(key), "key {key} used again");
        self.held.insert(key, super::Hold::File(path));
    }

    fn watch_directory(&mut self, key: u64, path: std::path::PathBuf) {
        assert!(self.keys.insert(key), "key {key} used again");
        self.held.insert(key, super::Hold::Directory(path));
    }

    fn unwatch(&mut self, key: u64) {
        assert!(self.held.remove(&key).is_some(), "key {key} let go twice");
        self.let_go.push(key);
    }

    fn deaf(&self) -> bool {
        self.deaf
    }

    fn now(&self) -> std::time::Instant {
        let ended = self.now.get() + self.taken;
        self.now.set(ended);
        ended
    }

    fn wait(
        &mut self,
        until: Option<std::time::Instant>,
        tells: &mut Vec<super::Tell>,
    ) -> Option<std::time::Instant> {
        let now = self.now.get();
        self.rests
            .push(until.map(|until| until.saturating_duration_since(now)));
        let turn = self.turns.pop_front()?;
        let told = turn(self);
        if told.is_empty() {
            let until = until.expect("a rest no clock ends and nothing tells never ends");
            self.now.set(until.max(now));
        }
        tells.extend(told);
        Some(self.now.get())
    }
}

/// What the watcher posted, in order.
#[derive(Clone, Default)]
struct Posts(std::rc::Rc<std::cell::RefCell<Vec<crate::Wake>>>);

impl Posts {
    /// The text of each config read posted.
    fn reads(&self) -> Vec<String> {
        self.0
            .borrow()
            .iter()
            .filter_map(|wake| match wake {
                crate::Wake::ConfigReloadObserved(observation) => Some(observation.text.clone()),
                _ => None,
            })
            .collect()
    }

    fn statuses(&self) -> Vec<WatchStatusEvent> {
        self.0
            .borrow()
            .iter()
            .filter_map(|wake| match wake {
                crate::Wake::ConfigWatchStatus(event) => Some(*event),
                _ => None,
            })
            .collect()
    }

    /// The text of each refused read posted.
    fn refused(&self) -> Vec<String> {
        self.0
            .borrow()
            .iter()
            .filter_map(|wake| match wake {
                crate::Wake::RefusedUpdateText(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn policies(&self) -> usize {
        self.0
            .borrow()
            .iter()
            .filter(|wake| matches!(wake, crate::Wake::HarnessPolicyObserved(_)))
            .count()
    }

    fn catalogs(&self) -> usize {
        self.0
            .borrow()
            .iter()
            .filter(|wake| matches!(wake, crate::Wake::ThemeCatalogChanged(_)))
            .count()
    }
}

/// Run the real watcher on `config` and `themes` over `script`, every post into `posts`.
fn run_watch(
    config: Option<&std::path::Path>,
    themes: Option<&std::path::Path>,
    script: &mut Script,
    posts: &Posts,
    repost: &super::Repost,
) {
    let initial = themes.map_or_else(
        || std::sync::Arc::new(crate::app_config::ThemeCatalog::default()),
        |dir| std::sync::Arc::new(crate::app_config::ThemeCatalog::try_discover_in(dir).unwrap()),
    );
    let sink = posts.clone();
    super::watch(
        config.map(std::path::Path::to_path_buf),
        None,
        initial,
        themes.map(std::path::Path::to_path_buf),
        &mut move |wake| {
            sink.0.borrow_mut().push(wake);
            true
        },
        script,
        repost,
    );
}

fn watch_scratch(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("aterm-config-watch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

/// At rest the watcher waits for a tell with no clock. A tell whose stamps did not move reads
/// nothing; a tell of an edit reads and posts it; a theme file's bytes rewritten in place,
/// which changes no entry of its folder, are held and told too.
#[test]
fn the_watcher_rests_untimed_and_reads_what_it_is_told() {
    let dir = watch_scratch("rest");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let themes = dir.join("themes");
    std::fs::create_dir(&themes).unwrap();
    let theme = themes.join("paper.conf");
    std::fs::write(&theme, "foreground = #202020\nbackground = #fafafa\n").unwrap();
    let posts = Posts::default();
    let seen = posts.clone();
    let (edited, conf, file) = (config.clone(), themes.clone(), theme.clone());
    let mut script = Script::new(
        false,
        vec![
            // The first theme step's holds: told at their first look, as a file watch does.
            Box::new(move |s: &mut Script| {
                let mut tells = s.on(&conf);
                tells.extend(s.on(&file));
                tells
            }),
            Box::new(move |s: &mut Script| {
                assert_eq!(s.rest(), None, "nothing to look at on a clock");
                assert_eq!(seen.reads().len(), 1, "the first read");
                s.on(&edited)
            }),
            Box::new({
                let seen = posts.clone();
                let edited = config.clone();
                move |s: &mut Script| {
                    assert_eq!(
                        seen.reads().len(),
                        1,
                        "a tell that moved no stamp reads nothing"
                    );
                    rewrite(&edited, "font_px = 16 # edited\n");
                    s.on(&edited)
                }
            }),
            Box::new({
                let seen = posts.clone();
                let file = theme.clone();
                move |s: &mut Script| {
                    assert_eq!(
                        seen.reads().last().map(String::as_str),
                        Some("font_px = 16 # edited\n")
                    );
                    assert_eq!(s.rest(), None);
                    std::fs::write(
                        &file,
                        "foreground = #101010\nbackground = #ffffff\ncursor = #303030\n",
                    )
                    .unwrap();
                    s.on(&file)
                }
            }),
        ],
    );
    run_watch(
        Some(&config),
        Some(&themes),
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    assert_eq!(posts.reads().len(), 2);
    assert_eq!(posts.catalogs(), 1, "the theme file's bytes, told");
    assert_eq!(script.rest(), None);
    assert_eq!(
        script.holds(),
        [
            super::Hold::File(config.clone()),
            super::Hold::File(theme.clone()),
            super::Hold::Directory(themes.clone()),
        ]
        .into_iter()
        .collect(),
        "the config, the theme folder and the theme file it read"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A config reached through links holds each one and the file they end at, in another folder:
/// that file edited in place, which changes nothing the link's folder holds, is read.
#[cfg(unix)]
#[test]
fn a_linked_config_holds_each_hop_and_reads_the_file_it_names_edited() {
    let dir = watch_scratch("links");
    let dotfiles = dir.join("dotfiles");
    let conf = dir.join("conf");
    std::fs::create_dir(&dotfiles).unwrap();
    std::fs::create_dir(&conf).unwrap();
    let target = dotfiles.join("aterm.toml");
    crate::test_scratch::write_config(&target, "font_px = 14\n");
    let middle = conf.join("middle.toml");
    std::os::unix::fs::symlink(&target, &middle).unwrap();
    let config = conf.join("aterm.toml");
    std::os::unix::fs::symlink("middle.toml", &config).unwrap();
    let posts = Posts::default();
    let edited = target.clone();
    let mut script = Script::new(
        false,
        vec![Box::new(move |s: &mut Script| {
            rewrite(&edited, "font_px = 18\n");
            s.on(&edited)
        })],
    );
    run_watch(
        Some(&config),
        None,
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    assert_eq!(
        script.holds(),
        [config.clone(), middle.clone(), target.clone()]
            .into_iter()
            .map(super::Hold::File)
            .collect(),
        "every hop"
    );
    assert_eq!(
        posts.reads().last().map(String::as_str),
        Some("font_px = 18\n")
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A refusal no edit ends (a file its user cannot read) is looked at again every
/// [`POLL_INTERVAL`], as the stat clock before the events looked, so a `chmod` macOS tells no
/// folder of is read within one; once readable, the config is read and the watcher rests
/// untimed.
#[cfg(unix)]
#[test]
fn a_refusal_no_edit_ends_is_looked_at_again_every_poll_interval() {
    use std::os::unix::fs::PermissionsExt as _;
    aterm_uds::host::could_not_run_as_root("a config of mode 000");
    let dir = watch_scratch("refused");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let posts = Posts::default();
    let mode = |path: &std::path::Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let mut turns: Vec<Turn> = vec![Box::new({
        let config = config.clone();
        move |s: &mut Script| {
            mode(&config, 0o000);
            s.on(&config)
        }
    })];
    for _ in 0..7 {
        turns.push(Box::new(|_: &mut Script| Vec::new()));
    }
    turns.push(Box::new({
        let config = config.clone();
        move |_: &mut Script| {
            mode(&config, 0o600);
            Vec::new()
        }
    }));
    let mut script = Script::new(false, turns);
    run_watch(
        Some(&config),
        None,
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    let ms = |ms| Some(std::time::Duration::from_millis(ms));
    assert_eq!(
        script.rests,
        [
            None,
            ms(500),
            ms(500),
            ms(500),
            ms(500),
            ms(500),
            ms(500),
            ms(500),
            ms(500),
            None,
        ],
        "no clock, then the poll interval's while refused, then none again"
    );
    assert!(
        posts
            .statuses()
            .contains(&WatchStatusEvent::Failed(WatchFailure {
                target: WatchTarget::Config,
                kind: WatchFailureKind::ConfigUnreadable,
            }))
    );
    assert_eq!(
        posts.reads().len(),
        2,
        "read at start and once readable again"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A failure an edit ends (bytes that are not UTF-8) sets no clock of its own: the edit that
/// ends it is told, and read. Its text gets one look more, for a second read to agree with it
/// ([`super::post_refused_read`]), and none after that.
#[test]
fn a_failure_an_edit_ends_waits_for_the_edit() {
    let dir = watch_scratch("edit-ends");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let posts = Posts::default();
    let mut script = Script::new(
        false,
        vec![
            Box::new({
                let config = config.clone();
                move |s: &mut Script| {
                    rewrite(&config, b"# caf\xe9\n");
                    s.on(&config)
                }
            }),
            Box::new(|s: &mut Script| {
                assert_eq!(
                    s.rest(),
                    Some(POLL_INTERVAL),
                    "one look, for the second read"
                );
                Vec::new()
            }),
            Box::new({
                let config = config.clone();
                move |s: &mut Script| {
                    assert_eq!(s.rest(), None, "no clock for a failure an edit ends");
                    rewrite(&config, "font_px = 15\n");
                    s.on(&config)
                }
            }),
        ],
    );
    run_watch(
        Some(&config),
        None,
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    assert!(
        posts
            .statuses()
            .contains(&WatchStatusEvent::Failed(WatchFailure {
                target: WatchTarget::Config,
                kind: WatchFailureKind::ConfigInvalidUtf8,
            }))
    );
    assert_eq!(
        posts.reads().last().map(String::as_str),
        Some("font_px = 15\n")
    );
    assert_eq!(script.rest(), None);
    let _ = std::fs::remove_dir_all(dir);
}

/// A seamless successor's Commit wakes the resting watcher (its [`super::Repost`] tells the
/// ring it listens on), which posts the policy, the config and the theme catalog again, as
/// they stand: the window dropped every wake before it.
#[test]
fn a_commit_wakes_the_watcher_to_post_everything_again() {
    let dir = watch_scratch("repost");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let themes = dir.join("themes");
    std::fs::create_dir(&themes).unwrap();
    let posts = Posts::default();
    let repost = std::sync::Arc::new(super::Repost::new());
    let (ring, heard) = std::sync::mpsc::channel();
    repost.listen(ring);
    let raised = std::sync::Arc::clone(&repost);
    let seen = posts.clone();
    let mut script = Script::new(
        false,
        vec![Box::new(move |_: &mut Script| {
            assert_eq!(
                (seen.policies(), seen.reads().len(), seen.catalogs()),
                (1, 1, 0),
                "control: one of each read at the start, and the catalog as it stood"
            );
            raised.raise();
            heard.try_iter().collect()
        })],
    );
    run_watch(Some(&config), Some(&themes), &mut script, &posts, &repost);
    assert_eq!(
        (posts.policies(), posts.reads().len(), posts.catalogs()),
        (2, 2, 1),
        "each posted again"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The file watch's thread ending turns the watcher to its clock: it looks every
/// [`POLL_INTERVAL`], so an edit no event tells is read at the next look.
#[test]
fn events_ended_turns_the_watcher_to_its_clock() {
    let dir = watch_scratch("ended");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let posts = Posts::default();
    let mut script = Script::new(
        false,
        vec![
            Box::new(|_: &mut Script| vec![super::Tell::EventsEnded]),
            Box::new({
                let config = config.clone();
                move |s: &mut Script| {
                    assert_eq!(s.rest(), Some(POLL_INTERVAL));
                    rewrite(&config, "font_px = 17 # edited\n");
                    Vec::new()
                }
            }),
        ],
    );
    run_watch(
        Some(&config),
        None,
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    assert_eq!(
        posts.reads().last().map(String::as_str),
        Some("font_px = 17 # edited\n")
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A watcher that hears nothing looks on today's clock: the config every [`POLL_INTERVAL`]
/// from the start, and an edit is read at the next look.
#[test]
fn deaf_ears_look_every_poll_interval() {
    let dir = watch_scratch("deaf");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let posts = Posts::default();
    let mut script = Script::new(
        true,
        vec![
            Box::new({
                let config = config.clone();
                move |s: &mut Script| {
                    assert_eq!(s.rest(), Some(POLL_INTERVAL));
                    rewrite(&config, "font_px = 19 # edited\n");
                    Vec::new()
                }
            }),
            Box::new(|s: &mut Script| {
                assert_eq!(s.rest(), Some(POLL_INTERVAL));
                Vec::new()
            }),
        ],
    );
    run_watch(
        Some(&config),
        None,
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    assert_eq!(
        posts.reads().last().map(String::as_str),
        Some("font_px = 19 # edited\n")
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A key's hold changes only by what left its step's set and what came: a path still wanted
/// keeps its key, one let go is unwatched, and a path that comes again gets a new key, so a
/// tell queued for the old one cannot be taken for it.
#[test]
fn held_keys_change_by_the_difference_and_are_never_reused() {
    use super::{Ears as _, Hold, Owes};
    let file = |name: &str| Hold::File(std::path::PathBuf::from(format!("/scratch/{name}")));
    let mut ears = Script::new(false, Vec::new());
    let mut held = super::Held::default();
    held.hold(&mut ears, Owes::Config, [file("a"), file("b")].into());
    held.hold(&mut ears, Owes::Themes, [file("a")].into());
    let first = ears.held.clone();
    held.hold(&mut ears, Owes::Config, [file("b"), file("c")].into());
    assert_eq!(ears.let_go, [0], "a let go");
    assert_eq!(ears.held.get(&1), first.get(&1), "b kept its key");
    assert_eq!(
        ears.held.get(&2),
        Some(&file("a")),
        "the theme step's own a"
    );
    assert_eq!(held.owes(2), Some(Owes::Themes));
    held.hold(
        &mut ears,
        Owes::Config,
        [file("a"), file("b"), file("c")].into(),
    );
    assert_eq!(held.owes(0), None, "the old key is no one's");
    assert_eq!(
        held.owes(4),
        Some(Owes::Config),
        "a came again with a new key"
    );
    assert!(!ears.deaf());
}

/// The retry clock: none for a success or a failure an edit ends, and [`POLL_INTERVAL`] after
/// each step while a failure that can end unheard goes on, a theme scan that changed as it
/// read among them.
#[test]
fn the_retry_clock_runs_only_for_a_failure_that_can_end_unheard() {
    let start = std::time::Instant::now();
    let mut retry = super::Retry::default();
    retry.after(start, Some(WatchFailureKind::ConfigInvalidUtf8));
    assert_eq!(retry.at, None);
    let mut now = start;
    for kind in [
        WatchFailureKind::ConfigUnreadable,
        WatchFailureKind::ConfigUnreadable,
        WatchFailureKind::ThemeChangedWhileScanning,
        WatchFailureKind::ThemeEntryUnreadable,
    ] {
        retry.after(now, Some(kind));
        let at = retry.at.unwrap();
        assert_eq!(at - now, POLL_INTERVAL, "{kind:?}");
        assert!(!retry.due(at - std::time::Duration::from_nanos(1)) && retry.due(at));
        now = at;
    }
    retry.after(now, None);
    assert_eq!(retry.at, None, "a success");
    retry.after(now, Some(WatchFailureKind::ConfigChangedWhileReading));
    assert_eq!(retry.at, None, "a config edit's own event ends it");
}

/// A deaf watcher rests a whole [`POLL_INTERVAL`] after each look, however long the look
/// took: each clock counts from its own step's end, and none comes sooner than that interval
/// after the last look, so looks slower than their interval do not run back to back.
/// NEGATIVE CONTROL: dropping the floor and counting the clocks from the wake (`now` for
/// `ended`) leaves three-second looks no rest, and a rest reads zero.
#[test]
fn a_deaf_watcher_rests_after_each_slow_look() {
    let dir = watch_scratch("deaf-slow");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let themes = dir.join("themes");
    std::fs::create_dir(&themes).unwrap();
    let posts = Posts::default();
    let mut turns: Vec<Turn> = Vec::new();
    for _ in 0..6 {
        turns.push(Box::new(|_: &mut Script| Vec::new()));
    }
    let mut script = Script::new(true, turns);
    script.taken = std::time::Duration::from_secs(3);
    run_watch(
        Some(&config),
        Some(&themes),
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    assert_eq!(script.rests.len(), 7);
    for rest in &script.rests {
        assert!(
            rest.is_some_and(|rest| rest >= POLL_INTERVAL),
            "every rest is a whole interval: {:?}",
            script.rests
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A FILE REFUSED FOR WHAT AN EDIT ENDS STILL REACHES THE WINDOW: the watcher rests on file
/// events, and a refusal an edit ends (bytes that are not UTF-8) set no clock, so an edit was
/// read once and its text, which waits for a second read to agree, was never sent. A person's
/// `auto_apply = false` added there held nothing in the window. CONTROL: without the
/// confirming look the rest has no clock, which the script refuses.
#[test]
fn a_refused_edit_is_read_again_until_a_second_read_agrees() {
    let dir = watch_scratch("refused-edit");
    let config = dir.join("aterm.toml");
    std::fs::write(&config, b"# caf\xe9\nfont_px = 14\n").unwrap();
    let says_off = b"# caf\xe9\n[update]\nauto_apply = false\n";
    let posts = Posts::default();
    let edited = config.clone();
    let mut script = Script::new(
        false,
        vec![
            Box::new(move |s: &mut Script| {
                assert_eq!(s.rest(), None, "CONTROL: nothing waits on a clock yet");
                rewrite(&edited, says_off);
                s.on(&edited)
            }),
            Box::new({
                let seen = posts.clone();
                move |s: &mut Script| {
                    assert_eq!(seen.refused(), Vec::<String>::new(), "one read is no text");
                    assert_eq!(s.rest(), Some(POLL_INTERVAL), "a second read is due");
                    Vec::new()
                }
            }),
        ],
    );
    run_watch(
        Some(&config),
        None,
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    assert_eq!(
        posts.refused(),
        [String::from_utf8_lossy(says_off).into_owned()],
        "the agreed text goes once"
    );
    assert_eq!(script.rest(), None, "and nothing waits on a clock after it");
    assert!(posts.reads().is_empty(), "CONTROL: nothing was admitted");
    let _ = std::fs::remove_dir_all(dir);
}

/// The watcher over this platform's file events reads an edit it hears, and ends once the
/// window refuses its posts.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn the_platform_watcher_reads_an_edit_it_hears_and_ends_with_the_window() {
    let dir = watch_scratch("platform");
    let config = dir.join("aterm.toml");
    crate::test_scratch::write_config(&config, "font_px = 14\n");
    let (posted, posts) = std::sync::mpsc::channel::<String>();
    let path = config.clone();
    let watcher = std::thread::spawn(move || {
        let (ring, tells) = std::sync::mpsc::channel();
        let repost = super::Repost::new();
        repost.listen(ring.clone());
        let mut ears = super::platform_ears(ring, tells);
        assert!(!ears.deaf(), "this platform's file events started");
        super::watch(
            Some(path),
            None,
            std::sync::Arc::new(crate::app_config::ThemeCatalog::default()),
            None,
            &mut move |wake| match wake {
                crate::Wake::ConfigReloadObserved(observation) => {
                    posted.send(observation.text).is_ok()
                }
                _ => true,
            },
            ears.as_mut(),
            &repost,
        );
    });
    let must = aterm_spec::wait::MUST_HAPPEN;
    assert_eq!(
        posts.recv_timeout(must).as_deref(),
        Ok("font_px = 14\n"),
        "the first read"
    );
    rewrite(&config, "font_px = 16\n");
    assert_eq!(
        posts.recv_timeout(must).as_deref(),
        Ok("font_px = 16\n"),
        "the edit, heard"
    );
    drop(posts);
    rewrite(&config, "font_px = 18\n");
    aterm_spec::wait::until_within(must, "the watcher ends with its posts refused", || {
        watcher.is_finished().then_some(())
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// A theme folder of more files than discovery reads holds a key on each file it reads, in
/// name order, and on none it never reads: past its count, or a name it does not take.
#[test]
fn a_theme_folder_holds_only_the_files_discovery_reads() {
    let dir = watch_scratch("theme-cap");
    let themes = dir.join("themes");
    std::fs::create_dir(&themes).unwrap();
    let most = crate::app_config::MAX_USER_THEME_FILES;
    let shadowed = aterm_types::scheme::builtin_names()[0].to_owned();
    for name in (0..most + 2)
        .map(|n| format!("t{n:04}"))
        .chain([shadowed.clone()])
    {
        std::fs::write(
            themes.join(format!("{name}.conf")),
            "foreground = #202020\nbackground = #fafafa\n",
        )
        .unwrap();
    }
    let posts = Posts::default();
    // The first rest, with no turn, ends the run once the first theme step held its keys.
    let mut script = Script::new(false, Vec::new());
    run_watch(
        None,
        Some(&themes),
        &mut script,
        &posts,
        &super::Repost::new(),
    );
    let files: Vec<_> = script
        .holds()
        .into_iter()
        .filter_map(|hold| match hold {
            super::Hold::File(path) => Some(path),
            super::Hold::Directory(_) => None,
        })
        .collect();
    let expected: Vec<_> = (0..most)
        .map(|n| themes.join(format!("t{n:04}.conf")))
        .collect();
    assert_eq!(
        files, expected,
        "the first {most} it reads, and not {shadowed}.conf"
    );
    let _ = std::fs::remove_dir_all(dir);
}
