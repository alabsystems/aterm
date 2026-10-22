// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::activate::agents_checkpoint::{self, Event};
use crate::flow::Fetcher;
use crate::vendor_direct::lane::{self, Verdict, world};
use crate::vendor_direct::policy::{Policy, YankLatch};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct Interleave {
    parallel: AtomicBool,
    stages: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Interleave {
    fn wait<'a>(
        &self,
        stages: std::sync::MutexGuard<'a, (bool, bool)>,
        pending: impl FnMut(&mut (bool, bool)) -> bool,
        why: &str,
    ) -> std::sync::MutexGuard<'a, (bool, bool)> {
        let (stages, timeout) = self
            .changed
            .wait_timeout_while(stages, aterm_spec::wait::MUST_HAPPEN, pending)
            .unwrap();
        assert!(!timeout.timed_out(), "{why}");
        stages
    }

    fn checkpoint(&self, event: Event<'_>) {
        if !self.parallel.load(Ordering::Acquire) {
            return;
        }
        match event {
            Event::Read("codex") => {
                let mut stages = self.stages.lock().unwrap();
                stages.0 = true;
                self.changed.notify_all();
                drop(self.wait(stages, |s| !s.1, "Codex never wrote its primary"));
            }
            Event::Written("codex") | Event::Waiting("codex") => {
                self.stages.lock().unwrap().1 = true;
                self.changed.notify_all();
            }
            _ => {}
        }
    }
}

/// The same vendor documents in either execution mode. Only a dispatch that borrows the
/// shared fetcher enables the parallel schedule: Codex's head waits until Claude's real
/// rollback reconcile has captured Codex's old target, then its primary write releases
/// that reconcile. A serial dispatch performs the identical policy moves in table order.
struct ScheduledVendors {
    vendors: world::Fake,
    interleave: Arc<Interleave>,
}

impl Fetcher for ScheduledVendors {
    fn index_candidates(&self) -> Result<Vec<crate::Candidate>, String> {
        self.vendors.index_candidates()
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.vendors.pkg_manifest(repo, program, build)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        self.vendors.download(repo, asset, dest)
    }

    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        if program == "codex" && self.interleave.parallel.load(Ordering::Acquire) {
            drop(self.interleave.wait(
                self.interleave.stages.lock().unwrap(),
                |s| !s.0,
                "Claude never captured Codex's primary",
            ));
        }
        self.vendors.vendor_head(program, url, cap, if_none_match)
    }

    fn shared(&self) -> Option<&(dyn Fetcher + Sync)> {
        self.interleave.parallel.store(true, Ordering::Release);
        Some(self)
    }
}

fn install(
    layout: &crate::store::Layout,
    vendors: &world::Fake,
    trust: &lane::Trust,
    program: &str,
) {
    let outcome = lane::install_one(
        &lane::Lane {
            layout,
            fetcher: vendors,
            triple: world::triple(),
            policy: &Policy::default(),
            trust,
            legacy_index: None,
            asked: false,
            now: world_now(),
            download_turn: None,
        },
        crate::vendor_direct::spec(program).unwrap(),
        false,
    );
    assert!(
        matches!(outcome.verdict, Verdict::Installed { .. }),
        "{outcome:?}"
    );
}

fn yanked_plans_preserve_their_primary_and_twin(codex_has_previous: bool) {
    let layout = crate::testdir::layout("atpkg-effectful-vendor-plan-");
    let vendors = world::Fake::default();
    let trust = world::trust();
    vendors.publish_claude("2.1.281", &world::native_exe("Claude previous"));
    install(&layout, &vendors, &trust, "claude");
    vendors.publish_claude("2.1.282", &world::native_exe("Claude yanked"));
    install(&layout, &vendors, &trust, "claude");
    if codex_has_previous {
        vendors.publish_codex_default("0.157.0");
        install(&layout, &vendors, &trust, "codex");
    }
    vendors.publish_codex_default("0.157.1");
    install(&layout, &vendors, &trust, "codex");
    let channel = crate::manifest::Channel {
        name: "stable".into(),
        channel_build: 1,
        min_build: 0,
        min_build_by_program: Default::default(),
        yanked: vec!["claude@2.1.282".into(), "codex@0.157.1".into()],
        pin: Default::default(),
        pin_by_target: Default::default(),
    };
    YankLatch::update_from_fresh_index(&layout, 1, &channel, world_now()).unwrap();
    let installed = crate::active_builds(&layout);
    let interleave = Arc::new(Interleave::default());
    let at_checkpoint = interleave.clone();
    let _watch = agents_checkpoint::watch(
        &layout,
        Arc::new(move |event| at_checkpoint.checkpoint(event)),
    );
    let fetcher = ScheduledVendors {
        vendors,
        interleave,
    };
    let (pass, fresh) = vendor_plan(
        &layout,
        &fetcher,
        &claude_and_codex_wanted(),
        &installed,
        VendorLanes {
            update_installed: true,
            complete_the_set: false,
        },
        None,
        &world_seams(&trust),
    );
    assert!(
        fresh.pending.is_empty(),
        "both decisions move retained state only"
    );
    let expected_failures = u32::from(!codex_has_previous);
    assert_eq!(pass.failures, expected_failures);
    let rows = crate::status::read(&layout).unwrap().programs;
    assert!(
        rows["claude"].state.contains("rolled back"),
        "{:?}",
        rows["claude"]
    );
    let expected_claude = world::build_dir(&layout, "claude", "2.1.281").join("bin/claude");
    let claude = crate::store::ToolName::new("claude").unwrap();
    for shim in [layout.shim(&claude), layout.agent_shim(&claude)] {
        assert_eq!(
            crate::platform::resolve_shim(&shim),
            Some(expected_claude.clone())
        );
    }
    let codex = crate::store::ToolName::new("codex").unwrap();
    if codex_has_previous {
        assert!(
            rows["codex"].state.contains("rolled back"),
            "{:?}",
            rows["codex"]
        );
        let expected = world::build_dir(&layout, "codex", "0.157.0").join("bin/codex");
        for shim in [layout.shim(&codex), layout.agent_shim(&codex)] {
            assert_eq!(
                crate::platform::resolve_shim(&shim),
                Some(expected.clone()),
                "{shim:?}"
            );
        }
    } else {
        assert!(
            rows["codex"].state.starts_with("tombstoned:"),
            "{:?}",
            rows["codex"]
        );
        assert!(
            std::fs::read_to_string(layout.shim(&codex))
                .unwrap()
                .contains("yanked/revoked")
        );
        assert_eq!(crate::platform::resolve_shim(&layout.shim(&codex)), None);
        assert!(
            !layout.agent_shim(&codex).exists(),
            "no runnable twin of the yanked build"
        );
    }
}

#[test]
fn a_vendor_plan_cannot_resurrect_a_tombstone_from_another_plan() {
    yanked_plans_preserve_their_primary_and_twin(false);
}

#[test]
fn a_vendor_plan_cannot_undo_another_plans_rollback() {
    yanked_plans_preserve_their_primary_and_twin(true);
}

struct FreshBesideDangling {
    set: SetBesideVendors,
    stages: Arc<Interleave>,
}

impl Fetcher for FreshBesideDangling {
    fn index_candidates(&self) -> Result<Vec<crate::Candidate>, String> {
        self.set.index_candidates()
    }

    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.set.pkg_manifest(repo, program, build)
    }

    fn download(&self, repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        self.set.download(repo, asset, dest)
    }

    fn download_for(
        &self,
        program: &str,
        repo: &str,
        asset: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), String> {
        self.set.download_for(program, repo, asset, dest, cap)
    }

    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        etag: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        self.set.vendor_get(program, url, cap, etag)
    }

    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        etag: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        self.set.vendor_head(program, url, cap, etag)
    }

    fn vendor_content_length(
        &self,
        program: &str,
        url: &str,
    ) -> Result<u64, crate::flow::VendorFetchError> {
        self.set.vendor_content_length(program, url)
    }

    fn vendor_download(
        &self,
        program: &str,
        url: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), crate::flow::VendorFetchError> {
        if program == "codex" {
            drop(self.stages.wait(
                self.stages.stages.lock().unwrap(),
                |s| !s.0,
                "the set never captured the dangling primary",
            ));
        }
        self.set.vendor_download(program, url, dest, cap)
    }

    fn source_id(&self) -> String {
        self.set.source_id()
    }

    fn shared(&self) -> Option<&(dyn Fetcher + Sync)> {
        Some(self)
    }
}

/// An emptied store can still have shims naming its old missing trees. The real first-run
/// rider repairs one while the ALab set reconciles its captured old target. The lane's
/// write either completes before that read publishes, or waits behind the read's turn;
/// neither ordering may restore the missing target over the newly landed program.
#[test]
fn a_first_runs_agent_repair_survives_a_dangling_primary_read_by_the_set() {
    let registry = crate::testdir::dir("atpkg-dangling-rider-registry-");
    write_toolchain_registry_and(&registry, &[("ay", 18)]);
    let layout = crate::testdir::layout("atpkg-dangling-rider-store-");
    let codex = crate::store::ToolName::new("codex").unwrap();
    let old = world::build_dir(&layout, "codex", "0.157.0");
    crate::activate::install_tools_env(
        &layout,
        &old,
        std::slice::from_ref(&codex),
        crate::activate::Aliases::Off,
        &crate::vendor_direct::spec("codex").unwrap().shim_env(),
    )
    .unwrap();
    assert!(!old.exists());
    assert!(crate::active_builds(&layout).is_empty());
    let set = SetBesideVendors::new(&registry, &layout, false);
    set.vendors.publish_codex_default("0.157.1");
    let stages = Arc::new(Interleave::default());
    let checkpoint = stages.clone();
    let prefix = layout.prefix.clone();
    let owner = std::thread::current().id();
    let _watch = agents_checkpoint::watch(
        &layout,
        Arc::new(move |event| match event {
            Event::Read("codex")
                if std::thread::current().id() == owner
                    && crate::active_builds(&crate::store::Layout {
                        prefix: prefix.clone(),
                    })
                    .contains_key("ay") =>
            {
                let mut stages = checkpoint.stages.lock().unwrap();
                stages.0 = true;
                checkpoint.changed.notify_all();
                drop(checkpoint.wait(
                    stages,
                    |s| !s.1,
                    "the fresh agent neither wrote nor waited for publication",
                ));
            }
            Event::Written("codex") | Event::Waiting("codex") => {
                checkpoint.stages.lock().unwrap().1 = true;
                checkpoint.changed.notify_all();
            }
            _ => {}
        }),
    );
    let fetcher = FreshBesideDangling { set, stages };
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.path = Some(std::ffi::OsString::new());
    // This checkpoint deliberately parks the set's publication turn. Keep the
    // repairing program first in its lane so no unrelated agent waits behind that
    // turn before the repair can announce its own publication attempt.
    let cfg = crate::config::PackagesConfig {
        exclude: Some(agents_except(&["codex"])),
        ..Default::default()
    };
    assert_eq!(
        run_update_pass(
            &layout,
            &cfg,
            &fetcher,
            &std::collections::BTreeMap::new(),
            true,
            &seams,
        ),
        0
    );
    let target = world::build_dir(&layout, "codex", "0.157.1").join("bin/codex");
    for shim in [layout.shim(&codex), layout.agent_shim(&codex)] {
        assert_eq!(
            crate::platform::resolve_shim(&shim),
            Some(target.clone()),
            "{shim:?}"
        );
    }
    assert!(!old.exists(), "the missing old tree was never restored");
    assert!(
        crate::status::read(&layout).unwrap().programs["codex"]
            .state
            .starts_with("managed 0.157.1")
    );
}
