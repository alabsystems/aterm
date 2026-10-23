// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Snapshot install, service unit, and `lane run` self-check tests in a scratch home.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use atpkg_publish::error::Result;
use atpkg_publish::lane::install::{
    self, Built, InstallFlags, Lint, Machine, ServiceManager, SnapshotBuilder,
};
use atpkg_publish::lane::report::Transcript;
use atpkg_publish::lane::units::{self, UnitSpec};
use atpkg_publish::lane::{LaneKind, self_check};
use atpkg_publish::testkit::NOW;
use atpkg_publish::triple::Os;

use super::fixture::{Fx, git};

#[derive(Default)]
struct FakeBuild {
    n: Mutex<u32>,
    edit: Option<&'static str>,
}

impl SnapshotBuilder for FakeBuild {
    fn build(&self, repo: &Path, target_dir: &Path, _log: &Path) -> Result<Built> {
        let mut n = self.n.lock().expect("n");
        *n += 1;
        if let Some(rel) = self.edit {
            std::fs::write(repo.join(rel), format!("// edited during build {n}\n")).expect("edit");
        }
        let binary = target_dir.join("release").join(install::BINARY);
        std::fs::create_dir_all(binary.parent().expect("parent")).expect("mkdir");
        std::fs::write(&binary, format!("#!/bin/false\nbuild {n}\n")).expect("write");
        Ok(Built {
            binary,
            toolchain: "targo 1.99.0 (fixture) / trustc 1.99.0 (fixture)".into(),
        })
    }
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Recorder {
    fn calls(&self) -> Vec<String> {
        self.0.lock().expect("calls").clone()
    }
}

impl ServiceManager for Recorder {
    fn lint(&self, kind: LaneKind, paths: &[PathBuf]) -> Result<Lint> {
        self.0
            .lock()
            .expect("calls")
            .push(format!("lint {}", kind.as_str()));
        match kind {
            LaneKind::Alab => install::lint_plists(paths),
            LaneKind::Linux => Ok(Lint::NotRun("the recorder does not run systemd".into())),
        }
    }

    fn arm(&self, kind: LaneKind, paths: &[PathBuf]) -> Result<()> {
        self.0.lock().expect("calls").push(format!(
            "arm {} {}",
            kind.as_str(),
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        Ok(())
    }

    fn disarm(&self, kind: LaneKind) -> Result<()> {
        self.0
            .lock()
            .expect("calls")
            .push(format!("disarm {}", kind.as_str()));
        Ok(())
    }
}

struct Box_ {
    fx: Fx,
    rec: Recorder,
    machine: Machine,
}

fn machine(label: &str, os: Os) -> Box_ {
    let mut fx = Fx::new(label);
    let repo = fx.path("checkout");
    super::fixture::repo(&repo, 0);
    std::fs::create_dir_all(repo.join("crates/x")).expect("mkdir");
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::write(repo.join("crates/x/lib.rs"), "\n").expect("write");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "inputs"]);
    fx.ctx.repo_root = repo;
    let rec = Recorder::default();
    let machine = Machine {
        os,
        agents_dir: fx.path("home/Library/LaunchAgents"),
        systemd_dir: fx.path("home/.config/systemd/user"),
        config_home: fx.path("home/.config"),
        builder: Box::new(FakeBuild::default()),
        services: Box::new(rec.clone()),
    };
    Box_ { fx, rec, machine }
}

fn install(
    b: &Box_,
    kind: LaneKind,
    flags: InstallFlags,
) -> (Result<install::Installed>, Transcript) {
    let tx = Transcript::memory("lane install");
    (
        install::install_with(&b.fx.ctx, kind, flags, &b.machine, &tx),
        tx,
    )
}

#[test]
fn the_snapshot_self_check_pins_a_run_to_the_bytes_its_unit_names() {
    let b = machine("self-check", Os::Linux);
    let flags = InstallFlags {
        no_arm: true,
        ..InstallFlags::default()
    };
    let (r, _) = install(&b, LaneKind::Linux, flags);
    let done = r.expect("installed");
    let (exe, hex, state) = (&done.binary, done.sha256.clone(), &b.fx.ctx.state_dir);
    assert_eq!(
        self_check(exe, Some(&hex), false, state).expect("pinned"),
        Some(hex.clone())
    );
    assert_eq!(
        self_check(exe, Some(&hex.to_uppercase()), false, state).expect("case-blind"),
        Some(hex.clone())
    );
    let own = b.fx.path("target/release/atpkg-publish");
    std::fs::create_dir_all(own.parent().expect("parent")).expect("mkdir");
    std::fs::copy(exe, &own).expect("copy");
    let e = self_check(&own, Some(&hex), false, state).expect_err("not a snapshot");
    assert!(e.to_string().contains("is not a snapshot"), "{e}");
    let record = done.snapshot.join("INSTALLED-FROM");
    let text = std::fs::read_to_string(&record).expect("record");
    std::fs::write(&record, text.replace(&hex, &"0".repeat(64))).expect("rewrite");
    let e = self_check(exe, Some(&hex), false, state).expect_err("record mismatch");
    assert!(e.to_string().contains("records another sha256"), "{e}");
    std::fs::write(&record, text).expect("restore");
    std::fs::write(exe, b"replaced in place").expect("tamper");
    let e = self_check(exe, Some(&hex), false, state).expect_err("tampered");
    assert!(
        e.to_string().contains("the snapshot was replaced in place"),
        "{e}"
    );
    assert_eq!(
        self_check(exe, None, true, state).expect("a person's dry run"),
        None
    );
    let e = self_check(exe, None, false, state).expect_err("a signing run names its hash");
    assert!(e.to_string().contains("started with no --sha256"), "{e}");
}

fn spec(kind: LaneKind, root: &Path) -> UnitSpec {
    UnitSpec {
        kind,
        binary: root.join("state root/aterm-auto/lane.20261015T000000Z/atpkg-publish"),
        sha256: "cd".repeat(32),
        home: root.join("home"),
        state_dir: root.join("state root/aterm-auto"),
        xdg_config_home: root.join("home/.config"),
        shim_bin: root.join("home/Library/Application Support/aterm/pkg/bin"),
    }
}

#[test]
fn the_launchd_agent_runs_the_snapshot_with_its_hash() {
    let root = Path::new("/Users//o");
    let unit = units::launchd(&spec(LaneKind::Alab, root));
    assert_eq!(unit.name, "systems.alab.aterm-auto-alab.plist");
    let t = &unit.text;
    for want in [
        "<string>/Users//o/state root/aterm-auto/lane.20261015T000000Z/atpkg-publish</string>",
        "<string>run</string>\n\t\t<string>alab</string>\n\t\t<string>--sha256</string>",
        &format!("<string>{}</string>", "cd".repeat(32)),
        "<string>/Users//o/state root/aterm-auto/alab.log</string>",
        "<key>StartInterval</key>\n\t<integer>21600</integer>",
        "<key>RunAtLoad</key>\n\t<true/>",
        "<key>XDG_STATE_HOME</key>\n\t\t<string>/Users//o/state root</string>",
        "<key>XDG_CONFIG_HOME</key>\n\t\t<string>/Users//o/home/.config</string>",
        "<string>/Users//o/state root/aterm-auto/alab.launchd.log</string>",
    ] {
        assert!(t.contains(want), "{want:?} missing from\n{t}");
    }
    let path = t
        .split("<key>PATH</key>\n\t\t<string>")
        .nth(1)
        .and_then(|r| r.split("</string>").next())
        .expect("PATH");
    assert!(
        path.starts_with("/Users//o/home/Library/Application Support/aterm/pkg/bin:"),
        "{path}"
    );
    assert!(
        !path.contains(".cargo/bin"),
        "no stock proxies on the lane's PATH"
    );
    assert!(!t.contains("__"), "no placeholder survives: {t}");
    assert!(!t.contains("/tools/"), "the agent never runs a checkout");
}

#[cfg(target_os = "macos")]
#[test]
fn the_rendered_agent_lints_with_plutil() {
    let fx = Fx::new("plutil");
    let unit = units::launchd(&spec(LaneKind::Alab, &fx.path("r&d <x>")));
    let p = fx.path(&unit.name);
    std::fs::write(&p, &unit.text).expect("write");
    assert_eq!(install::lint_plists(&[p]).expect("lints"), Lint::Passed);
}

#[test]
fn the_systemd_units_run_the_snapshot_hourly_with_their_words_escaped() {
    let root = Path::new("/home/o%x$y");
    let svc = units::systemd_service(&spec(LaneKind::Linux, root));
    assert_eq!(svc.name, "aterm-auto-atpkg.service");
    let exec = svc
        .text
        .lines()
        .find(|l| l.starts_with("ExecStart="))
        .expect("ExecStart");
    assert_eq!(
        exec,
        format!(
            "ExecStart=\"/home/o%%x$$y/state root/aterm-auto/lane.20261015T000000Z/atpkg-publish\" \
             \"lane\" \"run\" \"linux\" \"--sha256\" \"{}\" \"--log\" \"/home/o%%x$$y/state \
             root/aterm-auto/atpkg.log\"",
            "cd".repeat(32)
        )
    );
    assert!(svc.text.contains("Type=oneshot") && svc.text.contains("TimeoutStartSec=4h"));
    assert!(svc.text.contains(
        "Environment=\"PATH=/home/o%%x$y/home/Library/Application Support/aterm/pkg/bin:"
    ));
    assert!(
        svc.text
            .contains("\"XDG_CONFIG_HOME=/home/o%%x$y/home/.config\""),
        "the store a run resolves is the one the installer did: {}",
        svc.text
    );
    assert!(
        !svc.text.contains("%h/aterm"),
        "the unit never runs a checkout"
    );
    let timer = units::systemd_timer();
    assert!(
        timer.text.contains("OnCalendar=hourly") && timer.text.contains("WantedBy=timers.target")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_rendered_units_verify_with_systemd_analyze() {
    let fx = Fx::new("systemd-analyze");
    let mut s = spec(LaneKind::Linux, &fx.path("r"));
    s.binary = fx.path("atpkg-publish");
    std::fs::write(&s.binary, "#!/bin/false\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&s.binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let paths: Vec<PathBuf> = units::render(&s)
        .iter()
        .map(|u| {
            let p = fx.path(&u.name);
            std::fs::write(&p, &u.text).expect("write");
            p
        })
        .collect();
    assert_eq!(
        install::lint_systemd(&paths).expect("verifies"),
        Lint::Passed
    );
}

#[test]
fn install_refuses_the_wrong_os_and_a_box_with_no_machine_identity() {
    let b = machine("install-refusals", Os::Linux);
    let (r, _) = install(&b, LaneKind::Alab, InstallFlags::default());
    assert!(
        r.expect_err("not macOS")
            .to_string()
            .contains("the alab lane runs on macOS")
    );
    std::fs::remove_file(&b.fx.ctx.identity.record).expect("rm record");
    let (r, _) = install(&b, LaneKind::Linux, InstallFlags::default());
    let e = r.expect_err("no record");
    assert!(
        e.to_string()
            .contains("no machine record at ~/.aterm/machine.toml — the lane signs"),
        "{e}"
    );
    std::fs::remove_file(&b.fx.ctx.identity.key).expect("rm key");
    let (r, _) = install(&b, LaneKind::Linux, InstallFlags::default());
    let e = r.expect_err("no key");
    assert!(
        e.to_string()
            .contains("no machine key at ~/.aterm/machine.key — the lane signs"),
        "{e}"
    );
    assert!(e.to_string().contains("--keyless"));
    let (r, _) = install(
        &b,
        LaneKind::Linux,
        InstallFlags {
            keyless: true,
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    r.expect("a keyless Linux builder stages on purpose");
}

#[test]
fn install_refuses_a_dirty_checkout_unless_told_and_records_it() {
    let b = machine("install-dirty", Os::Darwin);
    std::fs::write(b.fx.ctx.repo_root.join("crates/x/lib.rs"), "// edited\n").expect("edit");
    let (r, _) = install(&b, LaneKind::Alab, InstallFlags::default());
    let e = r.expect_err("dirty");
    assert!(
        e.to_string().contains("crates/x/lib.rs"),
        "the refusal names the file: {e}"
    );
    assert!(
        !b.fx.ctx.state_dir.join("lane").exists(),
        "nothing snapshotted"
    );
    let (r, _) = install(
        &b,
        LaneKind::Alab,
        InstallFlags {
            allow_dirty: true,
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    let done = r.expect("allowed");
    let from = std::fs::read_to_string(done.snapshot.join("INSTALLED-FROM")).expect("record");
    assert!(from.contains("(dirty: crates/x/lib.rs)"), "{from}");
}

#[test]
fn an_edit_that_lands_during_the_build_is_never_snapshotted() {
    let mut b = machine("install-race", Os::Darwin);
    b.machine.builder = Box::new(FakeBuild {
        edit: Some("crates/x/lib.rs"),
        ..FakeBuild::default()
    });
    let (r, _) = install(&b, LaneKind::Alab, InstallFlags::default());
    let e = r.expect_err("the tree moved under the build");
    assert!(
        e.to_string()
            .contains("changed while the snapshot was building"),
        "{e}"
    );
    assert!(
        !b.fx.ctx.state_dir.join("lane").exists(),
        "nothing snapshotted"
    );
    let (r, _) = install(
        &b,
        LaneKind::Alab,
        InstallFlags {
            allow_dirty: true,
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    assert!(
        r.expect_err("a dirty file edited again")
            .to_string()
            .contains("changed while the snapshot was building")
    );
    b.machine.builder = Box::new(FakeBuild::default());
    let (r, _) = install(
        &b,
        LaneKind::Alab,
        InstallFlags {
            allow_dirty: true,
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    r.expect("a still tree, dirty on purpose");
}

#[test]
fn install_refuses_units_whose_config_home_names_another_store() {
    let mut b = machine("install-config-home", Os::Linux);
    let other = b.fx.path("other-config");
    std::fs::create_dir_all(other.join("aterm")).expect("mkdir");
    std::fs::write(
        other.join("aterm/aterm.toml"),
        "[packages]\nprefix = \"~/elsewhere\"\n",
    )
    .expect("config");
    b.machine.config_home = other;
    let (r, _) = install(
        &b,
        LaneKind::Linux,
        InstallFlags {
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    let e = r.expect_err("a run would resolve another store");
    assert!(
        e.to_string()
            .contains("other-config names — set XDG_CONFIG_HOME the same way for both"),
        "{e}"
    );
    assert!(!b.fx.ctx.state_dir.join("lane").exists());
}

#[test]
fn install_snapshots_the_build_renders_its_hash_and_arms_it() {
    let b = machine("install-arm", Os::Darwin);
    let (r, _) = install(&b, LaneKind::Alab, InstallFlags::default());
    let done = r.expect("installed");
    let link = b.fx.ctx.state_dir.join("lane");
    assert_eq!(std::fs::read_link(&link).expect("link"), done.snapshot);
    assert_eq!(
        atpkg::tree::sha256_file(&done.binary).expect("hash"),
        done.sha256
    );
    let from = std::fs::read_to_string(done.snapshot.join("INSTALLED-FROM")).expect("record");
    let head = git(&b.fx.ctx.repo_root, &["rev-parse", "HEAD"]);
    assert!(
        from.starts_with(&format!(
            "commit {head} (not on origin/main when snapshotted)"
        )),
        "{from}"
    );
    assert!(from.contains("tool build: targo 1.99.0 (fixture)"));
    let unit = std::fs::read_to_string(&done.armed[0]).expect("armed unit");
    assert!(
        unit.contains(&done.binary.display().to_string()),
        "the unit names the snapshot"
    );
    assert!(unit.contains(&done.sha256));
    assert_eq!(
        done.armed[0],
        b.machine
            .agents_dir
            .join("systems.alab.aterm-auto-alab.plist")
    );
    if cfg!(target_os = "macos") {
        assert_eq!(done.lint, Lint::Passed);
    }
    assert_eq!(
        b.rec.calls(),
        [
            "lint alab".to_string(),
            format!("arm alab {}", done.armed[0].display())
        ]
    );
}

#[test]
fn a_reinstall_keeps_the_previous_snapshot_and_prunes_the_rest() {
    let mut b = machine("install-keep", Os::Linux);
    let flags = InstallFlags::default();
    let mut snapshots = Vec::new();
    for day in 0..3 {
        b.fx.ctx.now_unix = NOW + day * 86_400;
        let (r, _) = install(&b, LaneKind::Linux, flags);
        snapshots.push(r.expect("installed"));
    }
    assert!(!snapshots[0].snapshot.exists(), "the oldest is pruned");
    assert!(
        snapshots[1].snapshot.exists(),
        "the previous is kept for a rollback"
    );
    assert_eq!(
        snapshots[2].previous.as_deref(),
        Some(snapshots[1].snapshot.as_path())
    );
    assert_ne!(snapshots[1].sha256, snapshots[2].sha256);
    assert_eq!(
        b.rec
            .calls()
            .iter()
            .filter(|c| c.starts_with("arm"))
            .count(),
        3
    );
    let rendered = std::fs::read_to_string(&snapshots[2].rendered[0]).expect("rendered");
    assert!(rendered.contains(&snapshots[2].sha256));
}

#[test]
fn rendering_unarmed_snapshots_keeps_the_executable_an_armed_unit_names() {
    let mut b = machine("install-no-arm-retain", Os::Linux);
    let (r, _) = install(&b, LaneKind::Linux, InstallFlags::default());
    let armed = r.expect("armed");
    let unit_before = std::fs::read(&armed.armed[0]).expect("armed unit");
    for day in 1..=2 {
        b.fx.ctx.now_unix = NOW + day * 86_400;
        let (r, _) = install(
            &b,
            LaneKind::Linux,
            InstallFlags {
                no_arm: true,
                ..InstallFlags::default()
            },
        );
        r.expect("rendered without rearming");
    }
    assert_eq!(
        std::fs::read(&armed.armed[0]).expect("armed unit"),
        unit_before
    );
    assert_eq!(
        std::fs::read_link(b.fx.ctx.state_dir.join("lane")).expect("live link"),
        armed.snapshot
    );
    assert!(
        armed.binary.is_file(),
        "the armed unit must still be runnable"
    );
}

#[test]
fn reinstalling_with_the_same_clock_stamp_preserves_the_armed_snapshot_bytes() {
    let b = machine("install-same-stamp", Os::Linux);
    let (r, _) = install(&b, LaneKind::Linux, InstallFlags::default());
    let armed = r.expect("armed");
    let original = std::fs::read(&armed.binary).expect("original binary");
    let (r, _) = install(
        &b,
        LaneKind::Linux,
        InstallFlags {
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    r.expect("rendered without rearming");
    assert_eq!(
        std::fs::read(&armed.binary).expect("armed binary"),
        original
    );
}

#[test]
fn a_legacy_shell_alias_survives_unarmed_and_failed_installs_until_the_manager_switches() {
    struct FailedArm;

    impl ServiceManager for FailedArm {
        fn lint(&self, _kind: LaneKind, _paths: &[PathBuf]) -> Result<Lint> {
            Ok(Lint::Passed)
        }

        fn arm(&self, _kind: LaneKind, _paths: &[PathBuf]) -> Result<()> {
            Err(atpkg_publish::error::Error::refused("fixture arm failure"))
        }

        fn disarm(&self, _kind: LaneKind) -> Result<()> {
            Ok(())
        }
    }

    let mut b = machine("install-legacy-alias", Os::Linux);
    let alias = b.fx.ctx.state_dir.join("lane");
    let script = alias.join("tools/linux-auto-atpkg.sh");
    std::fs::create_dir_all(script.parent().expect("parent")).expect("legacy tools");
    std::fs::write(&script, b"legacy shell snapshot").expect("legacy script");
    let (r, _) = install(
        &b,
        LaneKind::Linux,
        InstallFlags {
            no_arm: true,
            ..InstallFlags::default()
        },
    );
    let staged = r.expect("rendered");
    assert_eq!(
        std::fs::read(&script).expect("old shell unit's path"),
        b"legacy shell snapshot"
    );
    b.machine.services = Box::new(FailedArm);
    let (r, _) = install(&b, LaneKind::Linux, InstallFlags::default());
    r.expect_err("arming failed");
    assert_eq!(
        std::fs::read(&script).expect("old shell unit's path"),
        b"legacy shell snapshot"
    );
    assert!(
        staged.binary.is_file(),
        "a failed arm never prunes snapshots"
    );
    let failed_binary = std::fs::read_dir(&b.fx.ctx.state_dir)
        .expect("snapshots")
        .map(|e| e.expect("entry").path())
        .find(|p| p != &staged.snapshot && p.join(install::BINARY).is_file())
        .expect("the failed arm's newly rendered snapshot is also retained")
        .join(install::BINARY);
    assert!(failed_binary.is_file());
    b.machine.services = Box::new(b.rec.clone());
    let (r, _) = install(&b, LaneKind::Linux, InstallFlags::default());
    let armed = r.expect("manager switched");
    assert_eq!(
        std::fs::read_link(alias).expect("new alias"),
        armed.snapshot
    );
    assert_eq!(
        std::fs::read(
            armed
                .previous
                .expect("legacy rollback")
                .join("tools/linux-auto-atpkg.sh")
        )
        .expect("legacy kept"),
        b"legacy shell snapshot"
    );
}

#[test]
fn install_refuses_while_a_lane_run_holds_the_lock_and_disarm_removes_the_units() {
    let b = machine("install-lock", Os::Darwin);
    let state = atpkg_publish::lane::state::LaneState::of(LaneKind::Alab, &b.fx.ctx.state_dir);
    state.ensure().expect("state");
    let held = aterm_update_core::FileLock::acquire(&state.lock).expect("lock");
    let (r, _) = install(&b, LaneKind::Alab, InstallFlags::default());
    assert!(
        r.expect_err("in flight")
            .to_string()
            .contains("a alab lane run is in flight")
    );
    drop(held);
    let (r, _) = install(&b, LaneKind::Alab, InstallFlags::default());
    let done = r.expect("installed");
    std::fs::write(
        b.machine
            .agents_dir
            .join("systems.alab.aterm-auto-vendor.plist"),
        "x",
    )
    .expect("retired agent");
    let e = install::disarm(&b.fx.ctx, LaneKind::Linux, &b.machine)
        .expect_err("a Mac runs no systemd lane to disarm");
    assert!(
        e.to_string().contains("the linux lane runs on Linux"),
        "{e}"
    );
    install::disarm(&b.fx.ctx, LaneKind::Alab, &b.machine).expect("disarmed");
    assert!(!done.armed[0].exists());
    assert!(
        !b.machine
            .agents_dir
            .join("systems.alab.aterm-auto-vendor.plist")
            .exists()
    );
    assert!(
        done.snapshot.exists(),
        "the snapshot is left for inspection"
    );
    assert_eq!(
        b.rec.calls().last().map(String::as_str),
        Some("disarm alab")
    );
}
