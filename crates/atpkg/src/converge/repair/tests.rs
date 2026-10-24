// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for `aterm pkg repair` probe and apply steps.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_converge::{Class, ItemReport, Mode, Outcome, Probe, Remedy, Scope};

use super::{Ground, Pass};
use crate::store::Layout;

struct Scratch {
    layout: crate::testdir::ScratchLayout,
    home: crate::testdir::Dir,
}

impl Scratch {
    fn new(label: &str) -> Self {
        Self {
            layout: crate::testdir::layout(&format!("atpkg-repair-{label}-")),
            home: crate::testdir::dir(&format!("atpkg-repair-home-{label}-")),
        }
    }

    fn rustup(&self) -> PathBuf {
        let rustup = self.home.join(".rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
        rustup
    }

    fn ground(&self, pass: Pass, exe: Option<&Path>) -> Arc<Ground> {
        Arc::new(Ground {
            layout: Layout {
                prefix: self.layout.prefix.clone(),
            },
            home: Some(self.home.to_path_buf()),
            exe: exe.map(Path::to_path_buf),
            rustup_home: Some(self.home.join(".rustup")),
            login_rc: None,
            pass,
        })
    }

    fn login_ground(&self, pass: Pass, login: &'static str) -> Arc<Ground> {
        let mut ground = Arc::unwrap_or_clone(self.ground(pass, None));
        ground.login_rc = Some(login);
        Arc::new(ground)
    }

    fn release_exe(&self) -> PathBuf {
        let macos = self.home.join("Applications/aterm.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        for name in ["aterm", "atpkg"] {
            std::fs::write(macos.join(name), b"#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(macos.join(name), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        macos.join("aterm").canonicalize().unwrap()
    }
}

fn item(ground: &Arc<Ground>, id: &str) -> Box<dyn aterm_converge::Item> {
    super::items(ground)
        .into_iter()
        .find(|item| item.id() == id)
        .unwrap_or_else(|| panic!("no repair item {id}"))
}

fn probe(ground: &Arc<Ground>, id: &str) -> Probe {
    item(ground, id).probe()
}

fn run(ground: &Arc<Ground>, ids: &[&str], mode: Mode) -> Vec<ItemReport> {
    let items: Vec<Box<dyn aterm_converge::Item>> = super::items(ground)
        .into_iter()
        .filter(|item| ids.contains(&item.id().as_str()))
        .collect();
    aterm_converge::run_scoped(
        &items,
        mode,
        Scope::REPAIR,
        &mut aterm_converge::NoJournal,
        &mut |_| {},
    )
    .expect("the repair items order")
}

fn row<'a>(rows: &'a [ItemReport], id: &str) -> &'a ItemReport {
    rows.iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("no row {id}: {rows:?}"))
}

fn is_change(probe: &Probe) -> bool {
    matches!(probe, Probe::Change { .. })
}

fn is_ok(probe: &Probe) -> bool {
    matches!(probe, Probe::Ok { .. })
}

fn install(layout: &Layout, program: &str, build: u64) -> PathBuf {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join(program);
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::install_shims(
        layout,
        &dir,
        &[program.to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    crate::activate::activate_build(layout, &dir).unwrap();
    dir
}

fn install_trust(layout: &Layout, build: u64) -> PathBuf {
    let dir = layout.build_dir("trust", build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("lib").join("rustlib")).unwrap();
    std::fs::write(
        dir.join("lib").join("libtrust.dylib"),
        format!("driver of {build}"),
    )
    .unwrap();
    for tool in ["trustc", "targo", "trustdoc", "tippy"] {
        std::fs::write(
            dir.join("bin").join(tool),
            format!("{tool} of build {build}"),
        )
        .unwrap();
    }
    crate::activate::atomic_symlink(&dir, &crate::seam::store_current(layout)).unwrap();
    dir
}

#[test]
fn every_repair_step_is_one_item_of_its_class() {
    let scratch = Scratch::new("classes");
    let doctor = scratch.ground(Pass::Doctor, None);
    let ids: Vec<String> = super::items(&doctor).iter().map(|i| i.id()).collect();
    assert_eq!(ids, super::IDS);
    let class = |ground: &Arc<Ground>, id| item(ground, id).class();
    for (id, want) in [
        (super::COPIED_COMMANDS, Class::Asked),
        (super::LINKS, Class::Relay),
        (super::HOOKS, Class::Relay),
        (super::RC, Class::Relay),
        (super::STUBS, Class::Relay),
        (super::RUSTUP_SEAM, Class::Relay),
        (super::RUSTUP_LOCAL, Class::Asked),
        (super::HELD_UPDATE, Class::Relay),
        (super::SHIMS, Class::Relay),
        (super::EXEC_ROOTS, Class::Relay),
        (super::STORE, Class::Install),
    ] {
        assert_eq!(class(&doctor, id), want, "{id}");
    }
    let repair = scratch.ground(Pass::Repair, None);
    assert_eq!(class(&repair, super::RC), Class::Asked);
    assert_eq!(item(&doctor, super::RC).after(), [super::HOOKS]);
    for id in [super::SHIMS, super::EXEC_ROOTS, super::RUSTUP_SEAM] {
        assert!(item(&doctor, id).refresh().is_none(), "{id}");
    }
}

#[test]
fn the_hooks_are_laid_and_read_back() {
    let scratch = Scratch::new("hooks");
    let ground = scratch.ground(Pass::Doctor, None);
    assert!(
        is_change(&probe(&ground, super::HOOKS)),
        "a fresh home has none"
    );
    let planned = run(&ground, &[super::HOOKS], Mode::Plan);
    assert_eq!(row(&planned, super::HOOKS).outcome, Outcome::Planned);
    assert!(
        !scratch.home.join(".aterm").exists(),
        "a plan writes nothing"
    );

    let rows = run(&ground, &[super::HOOKS], Mode::Apply);
    assert_eq!(
        row(&rows, super::HOOKS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert!(is_ok(&probe(&ground, super::HOOKS)), "read back");
    let zsh = scratch.home.join(".aterm/shell.d/00-atpkg.zsh");
    assert!(zsh.is_file());

    std::fs::write(&zsh, b"stale").unwrap();
    std::fs::set_permissions(&zsh, std::fs::Permissions::from_mode(0o600)).unwrap();
    let Probe::Change { from, .. } = probe(&ground, super::HOOKS) else {
        panic!("a stale hook is a change");
    };
    assert!(from.contains("00-atpkg.zsh"), "{from}");
    run(&ground, &[super::HOOKS], Mode::Apply);
    assert!(is_ok(&probe(&ground, super::HOOKS)));
}

#[test]
fn the_doctor_lays_no_shell_integration_from_another_bundle() {
    let scratch = Scratch::new("other-bundle");
    std::fs::write(scratch.home.join(".zshrc"), "export EDITOR=vi\n").unwrap();
    let macos = scratch
        .home
        .join("Applications/aterm-dev.app/Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(macos.join("aterm"), b"").unwrap();
    let exe = macos.join("aterm");
    let doctor = scratch.ground(Pass::Doctor, Some(&exe));
    let rows = run(&doctor, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(row(&rows, super::HOOKS).outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(
        row(&rows, super::RC).outcome,
        Outcome::NotApplicable,
        "{rows:?}"
    );
    assert!(!scratch.home.join(".aterm").exists(), "nothing laid");
    let repair = scratch.ground(Pass::Repair, Some(&exe));
    let rows = run(&repair, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(
        row(&rows, super::HOOKS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Changed, "{rows:?}");
}

#[test]
fn an_unwired_rc_is_wired_and_an_opted_out_one_is_laid_again_by_repair_alone() {
    let scratch = Scratch::new("rc");
    let zshrc = scratch.home.join(".zshrc");
    std::fs::write(&zshrc, "export EDITOR=vi\n").unwrap();
    let doctor = scratch.ground(Pass::Doctor, None);
    assert!(matches!(probe(&doctor, super::RC), Probe::Waits { .. }));
    let rows = run(&doctor, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Changed, "{rows:?}");
    assert!(
        std::fs::read_to_string(&zshrc)
            .unwrap()
            .contains(crate::hooks::RC_BEGIN)
    );
    assert!(
        row(&rows, super::RC)
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("appended atpkg's block to ~/.zshrc"))
    );

    std::fs::write(&zshrc, "export EDITOR=vi\n").unwrap();
    let Probe::Ok { now } = probe(&doctor, super::RC) else {
        panic!("doctor leaves an opted-out rc as the person left it");
    };
    assert!(now.contains("you deleted atpkg's block"), "{now}");
    let rows = run(&doctor, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Ok);
    assert_eq!(
        std::fs::read_to_string(&zshrc).unwrap(),
        "export EDITOR=vi\n",
        "left unwired"
    );

    let repair = scratch.ground(Pass::Repair, None);
    assert!(is_change(&probe(&repair, super::RC)));
    let rows = run(&repair, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Changed, "{rows:?}");
    assert!(
        row(&rows, super::RC)
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("re-laid atpkg's block in ~/.zshrc"))
    );
    assert!(is_ok(&probe(&repair, super::RC)), "read back");
}

#[test]
fn the_login_shell_s_absent_rc_is_made_and_an_opt_out_lifts_under_repair_alone() {
    let scratch = Scratch::new("rc-login");
    let zshrc = scratch.home.join(".zshrc");
    let doctor = scratch.login_ground(Pass::Doctor, ".zshrc");
    run(&doctor, &[super::HOOKS, super::RC], Mode::Plan);
    assert!(
        std::fs::symlink_metadata(&zshrc).is_err(),
        "a plan makes nothing"
    );
    run(&doctor, &[super::HOOKS], Mode::Apply);
    let Probe::Change { from, .. } = probe(&doctor, super::RC) else {
        panic!("an absent login rc is a change");
    };
    assert!(from.contains("~/.zshrc does not exist"), "{from}");
    assert!(
        std::fs::symlink_metadata(&zshrc).is_err(),
        "a probe makes nothing"
    );

    let rows = run(&doctor, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Changed, "{rows:?}");
    assert!(
        row(&rows, super::RC)
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("created ~/.zshrc")),
        "{rows:?}"
    );
    let made = std::fs::read_to_string(&zshrc).unwrap();
    assert!(made.starts_with(crate::hooks::RC_BEGIN), "{made:?}");
    for other in [".bashrc", ".bash_profile", ".config/fish/config.fish"] {
        assert!(!scratch.home.join(other).exists(), "{other} appeared");
    }
    assert!(is_ok(&probe(&doctor, super::RC)), "read back");

    std::fs::write(&zshrc, "").unwrap();
    run(&doctor, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), "");
    let repair = scratch.login_ground(Pass::Repair, ".zshrc");
    run(&repair, &[super::HOOKS, super::RC], Mode::Apply);
    assert!(
        std::fs::read_to_string(&zshrc)
            .unwrap()
            .contains(crate::hooks::RC_BEGIN)
    );

    std::fs::remove_file(&zshrc).unwrap();
    let Probe::Ok { now } = probe(&doctor, super::RC) else {
        panic!("doctor leaves a deleted login rc absent");
    };
    assert!(now.contains("~/.zshrc left absent"), "{now}");
    run(&doctor, &[super::HOOKS, super::RC], Mode::Apply);
    assert!(std::fs::symlink_metadata(&zshrc).is_err(), "left absent");
    assert!(is_change(&probe(&repair, super::RC)));
    let rows = run(&repair, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), made);
}

#[test]
fn the_rc_block_waits_for_the_hooks() {
    let scratch = Scratch::new("rc-after");
    std::fs::write(scratch.home.join(".zshrc"), "").unwrap();
    std::fs::write(scratch.home.join(".aterm"), b"not a directory").unwrap();
    let rows = run(
        &scratch.ground(Pass::Doctor, None),
        &[super::HOOKS, super::RC],
        Mode::Apply,
    );
    assert_eq!(
        row(&rows, super::HOOKS).outcome,
        Outcome::Failed,
        "{rows:?}"
    );
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Blocked);
    assert_eq!(
        std::fs::read_to_string(scratch.home.join(".zshrc")).unwrap(),
        "",
        "no block naming a hook that is not there"
    );

    let scratch = Scratch::new("rc-only");
    let zshrc = scratch.home.join(".zshrc");
    let original = "export EDITOR=vi\n";
    std::fs::write(&zshrc, original).unwrap();
    let ground = scratch.ground(Pass::Doctor, None);
    let narrowed = || {
        let (items, missed) =
            aterm_converge::select(super::items(&ground), &[super::RC.to_owned()]);
        assert!(missed.is_empty());
        assert_eq!(items.len(), 1);
        assert!(items[0].after().is_empty(), "the ordering edge is narrowed");
        aterm_converge::run_scoped(
            &items,
            Mode::Apply,
            Scope::REPAIR,
            &mut aterm_converge::NoJournal,
            &mut |_| {},
        )
        .unwrap()
    };
    let rows = narrowed();
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Blocked, "{rows:?}");
    assert!(item(&ground, super::RC).apply().is_err());
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), original);
    assert!(!scratch.home.join(".aterm").exists());

    run(&ground, &[super::HOOKS], Mode::Apply);
    let shell_d = scratch.home.join(".aterm/shell.d");
    let elsewhere = scratch.home.join("elsewhere");
    std::fs::rename(&shell_d, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &shell_d).unwrap();
    let rows = narrowed();
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Blocked, "{rows:?}");
    assert!(item(&ground, super::RC).apply().is_err());
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), original);

    std::fs::remove_file(&shell_d).unwrap();
    std::fs::rename(&elsewhere, &shell_d).unwrap();
    let rows = narrowed();
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Changed, "{rows:?}");
    assert!(is_ok(&probe(&ground, super::RC)), "read back");
    assert!(
        std::fs::read_to_string(&zshrc)
            .unwrap()
            .contains(crate::hooks::RC_BEGIN)
    );
}

#[test]
fn a_missing_command_link_is_laid_from_the_installed_app_and_read_back() {
    let scratch = Scratch::new("links");
    let exe = scratch.release_exe();
    let ground = scratch.ground(Pass::Doctor, Some(&exe));
    let link = scratch.home.join(".local/bin/aterm");
    assert!(is_change(&probe(&ground, super::LINKS)));
    let rows = run(&ground, &[super::LINKS], Mode::Apply);
    assert_eq!(
        row(&rows, super::LINKS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        exe.parent().unwrap().join("aterm")
    );
    assert!(is_ok(&probe(&ground, super::LINKS)), "read back");

    std::fs::remove_file(&link).unwrap();
    assert!(is_change(&probe(&ground, super::LINKS)));
    let dev = scratch.home.join("target/debug/aterm");
    std::fs::create_dir_all(dev.parent().unwrap()).unwrap();
    std::fs::write(&dev, b"").unwrap();
    let from_dev = scratch.ground(Pass::Doctor, Some(&dev));
    assert!(matches!(
        probe(&from_dev, super::LINKS),
        Probe::NotApplicable { .. }
    ));
    run(&from_dev, &[super::LINKS], Mode::Apply);
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "a dev build lays no link"
    );
}

#[test]
fn a_copied_command_is_moved_aside_by_the_installed_app_only() {
    let scratch = Scratch::new("copied");
    let bin = scratch.home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("aterm"), b"an old copy").unwrap();

    let dev = scratch.home.join("target/debug/aterm");
    std::fs::create_dir_all(dev.parent().unwrap()).unwrap();
    std::fs::write(&dev, b"").unwrap();
    let from_dev = scratch.ground(Pass::Repair, Some(&dev));
    let rows = run(&from_dev, &[super::COPIED_COMMANDS], Mode::Apply);
    let blocked = row(&rows, super::COPIED_COMMANDS);
    assert_eq!(blocked.outcome, Outcome::Blocked, "{rows:?}");
    assert!(
        matches!(
            blocked.explain.as_ref().map(|e| &e.remedy),
            Some(Remedy::Command(_))
        ),
        "{blocked:?}"
    );
    assert!(
        std::fs::symlink_metadata(bin.join("aterm"))
            .unwrap()
            .file_type()
            .is_file()
    );

    let exe = scratch.release_exe();
    let ground = scratch.ground(Pass::Repair, Some(&exe));
    assert!(is_change(&probe(&ground, super::COPIED_COMMANDS)));
    let rows = run(&ground, &[super::COPIED_COMMANDS], Mode::Apply);
    assert_eq!(
        row(&rows, super::COPIED_COMMANDS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        std::fs::read_link(bin.join("aterm")).unwrap(),
        exe.parent().unwrap().join("aterm")
    );
    let aside = std::fs::read_dir(&bin).unwrap().flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with("aterm.moved-aside-")
    });
    assert!(aside, "the copy keeps a name beside its old one");
    assert!(is_ok(&probe(&ground, super::COPIED_COMMANDS)), "read back");
}

#[test]
fn the_reroute_stubs_are_laid_and_read_back() {
    let scratch = Scratch::new("stubs");
    let ground = scratch.ground(Pass::Doctor, None);
    assert!(is_change(&probe(&ground, super::STUBS)), "none laid yet");
    let rows = run(&ground, &[super::STUBS], Mode::Apply);
    assert_eq!(
        row(&rows, super::STUBS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert!(is_ok(&probe(&ground, super::STUBS)), "read back");

    let cargo = crate::reroute::dir(&scratch.layout).join("cargo");
    std::fs::remove_file(&cargo).unwrap();
    let Probe::Change { from, .. } = probe(&ground, super::STUBS) else {
        panic!("a deleted stub is a change");
    };
    assert!(from.contains("cargo"), "{from}");
    run(&ground, &[super::STUBS], Mode::Apply);
    assert!(cargo.is_file(), "laid again");
}

#[test]
fn rustup_trust_is_attached_where_trust_is_installed_and_read_back() {
    let scratch = Scratch::new("seam");
    let rustup = scratch.rustup();
    let ground = scratch.ground(Pass::Doctor, None);
    assert!(
        matches!(
            probe(&ground, super::RUSTUP_SEAM),
            Probe::NotApplicable { .. }
        ),
        "no Trust installed: no entry wanted"
    );
    install_trust(&scratch.layout, 6808);
    assert!(is_change(&probe(&ground, super::RUSTUP_SEAM)));
    let rows = run(&ground, &[super::RUSTUP_SEAM], Mode::Apply);
    assert_eq!(
        row(&rows, super::RUSTUP_SEAM).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        std::fs::read_link(crate::seam::seam_path(&rustup, "trust")).unwrap(),
        crate::seam::seam_target(&scratch.layout, "trust")
    );
    assert!(is_ok(&probe(&ground, super::RUSTUP_SEAM)), "read back");
}

#[test]
fn a_dangling_rustup_trust_link_is_re_pointed_at_the_store_and_read_back() {
    let scratch = Scratch::new("local");
    let rustup = scratch.rustup();
    install_trust(&scratch.layout, 6808);
    let entry = crate::seam::seam_path(&rustup, "trust");
    std::os::unix::fs::symlink(scratch.home.join("toolchains/gone"), &entry).unwrap();
    let ground = scratch.ground(Pass::Repair, None);
    let Probe::Change { from, .. } = probe(&ground, super::RUSTUP_LOCAL) else {
        panic!("a link to nothing is stale");
    };
    assert!(from.contains("no longer exists"), "{from}");
    let rows = run(&ground, &[super::RUSTUP_LOCAL], Mode::Apply);
    assert_eq!(
        row(&rows, super::RUSTUP_LOCAL).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        std::fs::read_link(&entry).unwrap(),
        crate::seam::seam_target(&scratch.layout, "trust")
    );
    assert!(is_ok(&probe(&ground, super::RUSTUP_LOCAL)), "read back");
}

#[test]
fn an_outlived_held_update_is_ended_and_read_back() {
    let scratch = Scratch::new("held");
    let from: std::collections::BTreeMap<String, u64> = [("clean".to_string(), 7)].into();
    let to = from.keys().map(|p| (p.clone(), 99u64)).collect();
    assert!(crate::quiet::note_deferred(
        &scratch.layout,
        "rustc",
        1_790_000_000,
        1_790_000_000,
        &crate::quiet::Busy::Unknown {
            why: String::from("the process table could not be read"),
        },
        &crate::quiet::Moves {
            from,
            to,
            assets: std::collections::BTreeMap::new(),
        },
    ));
    let ground = scratch.ground(Pass::Doctor, None);
    assert!(
        is_change(&probe(&ground, super::HELD_UPDATE)),
        "no build it moves off"
    );
    let rows = run(&ground, &[super::HELD_UPDATE], Mode::Apply);
    assert_eq!(
        row(&rows, super::HELD_UPDATE).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert!(crate::quiet::read(&scratch.layout).is_none());
    assert!(is_ok(&probe(&ground, super::HELD_UPDATE)), "read back");
}

#[test]
fn a_deleted_shim_is_restored_and_read_back() {
    let scratch = Scratch::new("shims");
    let build = install(&scratch.layout, "ay", 17);
    let ground = scratch.ground(Pass::Doctor, None);
    assert!(is_ok(&probe(&ground, super::SHIMS)), "laid by the install");
    let ay = crate::store::ToolName::new("ay").unwrap();
    std::fs::remove_file(scratch.layout.shim(&ay)).unwrap();
    let Probe::Change { from, .. } = probe(&ground, super::SHIMS) else {
        panic!("a deleted shim is a change");
    };
    assert!(from.contains("ay"), "{from}");
    let planned = run(&ground, &[super::SHIMS], Mode::Plan);
    assert_eq!(row(&planned, super::SHIMS).outcome, Outcome::Planned);
    assert!(
        std::fs::symlink_metadata(scratch.layout.shim(&ay)).is_err(),
        "a plan lays nothing"
    );

    let rows = run(&ground, &[super::SHIMS], Mode::Apply);
    assert_eq!(
        row(&rows, super::SHIMS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    let target = crate::platform::resolve_shim(&scratch.layout.shim(&ay)).unwrap();
    assert!(target.starts_with(&build), "{}", target.display());
    assert!(is_ok(&probe(&ground, super::SHIMS)), "read back");
}

#[test]
fn a_build_whose_every_recorded_command_another_file_holds_is_left_alone() {
    let scratch = Scratch::new("held");
    let build = install(&scratch.layout, "ay", 17);
    let ay = crate::store::ToolName::new("ay").unwrap();
    crate::activate::install_tools(
        &scratch.layout,
        &build,
        std::slice::from_ref(&ay),
        crate::activate::Aliases::Alab,
    )
    .unwrap();
    crate::store::write_exposes(&build, std::slice::from_ref(&ay)).unwrap();
    let mine = b"#!/bin/sh\necho mine\n";
    std::fs::remove_file(scratch.layout.shim(&ay)).unwrap();
    std::fs::write(scratch.layout.shim(&ay), mine).unwrap();
    let ground = scratch.ground(Pass::Repair, None);
    assert!(
        is_ok(&probe(&ground, super::STORE)),
        "an installed program, not none"
    );

    let relaid = super::relay(&scratch.layout);
    assert_eq!(relaid.relaid, 0, "{:?}", relaid.notes);
    assert!(relaid.failed.is_empty(), "{:?}", relaid.failed);
    assert!(
        relaid
            .notes
            .iter()
            .any(|n| n.contains("ay@17: every command it exposes is held by another file")),
        "{:?}",
        relaid.notes
    );
    assert_eq!(std::fs::read(scratch.layout.shim(&ay)).unwrap(), mine);
    let alias = scratch.layout.shim(&ay.alias().unwrap());
    let target = crate::platform::resolve_shim(&alias).expect("the alias survives");
    assert!(target.starts_with(&build), "{}", target.display());
}

#[test]
fn a_re_lay_keeps_the_alias_of_a_recorded_command_a_file_of_the_users_holds() {
    let scratch = Scratch::new("held-partial");
    let build = install(&scratch.layout, "ay", 17);
    let tools = ["ay", "ay-pb"].map(|name| crate::store::ToolName::new(name).unwrap());
    let [ay, ay_pb] = &tools;
    let ay_pb_exe = build.join("bin").join("ay-pb");
    std::fs::write(&ay_pb_exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&ay_pb_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::activate::install_tools(
        &scratch.layout,
        &build,
        &tools,
        crate::activate::Aliases::Alab,
    )
    .unwrap();
    crate::store::write_exposes(&build, &tools).unwrap();
    let mine = b"#!/bin/sh\necho mine\n";
    std::fs::remove_file(scratch.layout.shim(ay_pb)).unwrap();
    std::fs::write(scratch.layout.shim(ay_pb), mine).unwrap();
    std::fs::remove_file(scratch.layout.shim(ay)).unwrap();

    let relaid = super::relay(&scratch.layout);
    assert_eq!(relaid.relaid, 1, "{:?}", relaid.notes);
    assert!(relaid.failed.is_empty(), "{:?}", relaid.failed);
    let ay_target = crate::platform::resolve_shim(&scratch.layout.shim(ay)).expect("ay re-laid");
    assert!(ay_target.starts_with(&build), "{}", ay_target.display());
    assert_eq!(std::fs::read(scratch.layout.shim(ay_pb)).unwrap(), mine);
    let alias = scratch.layout.shim(&ay_pb.alias().unwrap());
    let target = crate::platform::resolve_shim(&alias)
        .expect("the alias, now the one way to run the build's ay-pb, survives the re-lay");
    assert!(target.starts_with(&build), "{}", target.display());
}

#[test]
fn a_build_that_records_no_commands_is_neither_laid_nor_said_held() {
    let scratch = Scratch::new("records-none");
    let build = install(&scratch.layout, "ay", 17);
    crate::store::write_exposes(&build, &[]).unwrap();

    let relaid = super::relay(&scratch.layout);
    assert_eq!(relaid.relaid, 0, "{:?}", relaid.notes);
    assert!(relaid.failed.is_empty(), "{:?}", relaid.failed);
    assert!(
        !relaid
            .notes
            .iter()
            .any(|n| n.contains("held by another file")),
        "{:?}",
        relaid.notes
    );
}

#[test]
fn a_shim_laid_under_a_now_refused_name_is_removed_and_read_back() {
    let scratch = Scratch::new("refused-shims");
    install(&scratch.layout, "ay", 17);
    install(&scratch.layout, "ny", 3);
    let ground = scratch.ground(Pass::Doctor, None);
    assert!(is_ok(&probe(&ground, super::SHIMS)), "laid by the install");
    let bin = scratch.layout.bin_dir();
    let ay = crate::store::ToolName::new("ay").unwrap();
    let ny = crate::store::ToolName::new("ny").unwrap();
    std::fs::copy(scratch.layout.shim(&ay), bin.join("pwsh")).unwrap();
    std::fs::copy(scratch.layout.shim(&ny), bin.join("fish")).unwrap();
    let theirs = scratch.home.join("my-zsh");
    std::fs::write(&theirs, b"#!/bin/sh\n").unwrap();
    std::os::unix::fs::symlink(&theirs, bin.join("zsh")).unwrap();
    std::fs::write(bin.join("csh"), b"#!/bin/sh\necho mine\n").unwrap();
    std::os::unix::fs::symlink(
        scratch.layout.build_dir("gone", 5).join("bin/ksh"),
        bin.join("ksh"),
    )
    .unwrap();
    let Probe::Change { from, .. } = probe(&ground, super::SHIMS) else {
        panic!("a refused-name shim is a change");
    };
    assert!(
        from.contains("bin/ shims under a name no shim may carry: fish, ksh, pwsh"),
        "{from}"
    );
    let planned = run(&ground, &[super::SHIMS], Mode::Plan);
    assert_eq!(row(&planned, super::SHIMS).outcome, Outcome::Planned);
    assert!(bin.join("pwsh").is_file(), "a plan removes nothing");

    let rows = run(&ground, &[super::SHIMS], Mode::Apply);
    let applied = row(&rows, super::SHIMS);
    assert_eq!(applied.outcome, Outcome::Changed, "{rows:?}");
    assert!(
        applied
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("under a name no shim may carry: fish, ksh, pwsh")),
        "{applied:?}"
    );
    for gone in ["pwsh", "fish", "ksh"] {
        assert!(
            std::fs::symlink_metadata(bin.join(gone)).is_err(),
            "{gone} is removed"
        );
    }
    assert_eq!(std::fs::read_link(bin.join("zsh")).unwrap(), theirs);
    assert!(bin.join("csh").is_file());
    assert!(is_ok(&probe(&ground, super::SHIMS)), "read back");
}

fn trust_needing_a_root(layout: &Layout) -> PathBuf {
    let dir = layout.build_dir("trust", 8595);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(
        dir.join("lib").join("librustc_driver-5cfd.dylib"),
        b"driver",
    )
    .unwrap();
    for tool in ["targo", "tippy", "targo-tippy", "tippy-driver"] {
        std::fs::write(bin.join(tool), format!("{tool} 8595")).unwrap();
    }
    std::fs::write(bin.join("trustc"), b"frontend / signature: trustc").unwrap();
    std::fs::write(bin.join("rustc"), b"frontend / signature: rustc_").unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::atomic_symlink(&dir, &layout.program_current("trust")).unwrap();
    crate::activate::activate_build(layout, &dir).unwrap();
    layout.ensure_dir(&layout.bin_dir()).unwrap();
    for (name, tool) in [("trustc", "trustc"), ("targo", "targo"), ("tippy", "tippy")] {
        let shim = layout.bin_dir().join(name);
        std::fs::write(
            &shim,
            crate::platform::sh_shim_content_env(&bin.join(tool), &crate::shim_env::ShimEnv::NONE),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

#[test]
fn the_exec_root_is_laid_and_the_trust_shims_routed_and_read_back() {
    let scratch = Scratch::new("exec-roots");
    let layout: &Layout = &scratch.layout;
    trust_needing_a_root(layout);

    let ground = scratch.ground(Pass::Doctor, None);
    assert!(is_change(&probe(&ground, super::EXEC_ROOTS)), "no root yet");
    let rows = run(&ground, &[super::EXEC_ROOTS], Mode::Apply);
    assert_eq!(
        row(&rows, super::EXEC_ROOTS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert!(
        crate::compat::root_dir(layout, 8595)
            .join("bin/rustc")
            .is_file()
    );
    assert!(is_ok(&probe(&ground, super::EXEC_ROOTS)), "read back");
}

#[test]
fn a_withdrawn_build_is_blocked_on_an_update_and_its_tombstone_kept() {
    let scratch = Scratch::new("store");
    let ground = scratch.ground(Pass::Repair, None);
    assert!(matches!(
        probe(&ground, super::STORE),
        Probe::NotApplicable { .. }
    ));
    install(&scratch.layout, "ay", 17);
    assert!(is_ok(&probe(&ground, super::STORE)));
    let ay = crate::store::ToolName::new("ay").unwrap();
    crate::activate::install_tombstone_shim(&scratch.layout, &ay).unwrap();
    let rows = run(&ground, &[super::SHIMS, super::STORE], Mode::Apply);
    let withdrawn = row(&rows, super::STORE);
    assert_eq!(withdrawn.outcome, Outcome::Blocked, "{rows:?}");
    assert!(
        withdrawn
            .why
            .as_deref()
            .is_some_and(|w| w.contains("ALab withdrew ay@17")),
        "{withdrawn:?}"
    );
    assert_eq!(
        withdrawn.explain.as_ref().map(|e| &e.remedy),
        Some(&Remedy::Command("aterm pkg update".to_owned()))
    );
    assert!(
        crate::platform::resolve_shim(&scratch.layout.shim(&ay)).is_none(),
        "no shim laid over the tombstone"
    );
}

#[test]
fn a_link_into_a_moved_bundle_is_re_pointed_and_read_back() {
    let scratch = Scratch::new("moved-bundle");
    let exe = scratch.release_exe();
    let bin = scratch.home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gone = scratch
        .home
        .join("Downloads/aterm.app/Contents/MacOS/aterm");
    std::os::unix::fs::symlink(&gone, bin.join("aterm")).unwrap();
    std::os::unix::fs::symlink(exe.parent().unwrap().join("atpkg"), bin.join("atpkg")).unwrap();
    let ground = scratch.ground(Pass::Doctor, Some(&exe));
    let Probe::Change { from, .. } = probe(&ground, super::LINKS) else {
        panic!("a link into a bundle that moved is a change");
    };
    assert!(from.contains("Downloads/aterm.app"), "{from}");
    let rows = run(&ground, &[super::LINKS], Mode::Apply);
    assert_eq!(
        row(&rows, super::LINKS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        std::fs::read_link(bin.join("aterm")).unwrap(),
        exe.parent().unwrap().join("aterm"),
        "re-pointed at the installed app"
    );
    assert!(is_ok(&probe(&ground, super::LINKS)), "read back");

    std::fs::remove_file(bin.join("aterm")).unwrap();
    let mine = scratch.home.join("src/aterm/target/release/aterm");
    std::os::unix::fs::symlink(&mine, bin.join("aterm")).unwrap();
    assert!(is_ok(&probe(&ground, super::LINKS)), "theirs");
    run(&ground, &[super::LINKS], Mode::Apply);
    assert_eq!(std::fs::read_link(bin.join("aterm")).unwrap(), mine);
}

#[test]
fn a_loose_shell_dir_is_hardened_and_a_symlinked_one_holds_the_rc_back() {
    use std::os::unix::fs::MetadataExt as _;
    let scratch = Scratch::new("loose-dir");
    let ground = scratch.ground(Pass::Doctor, None);
    run(&ground, &[super::HOOKS], Mode::Apply);
    assert!(is_ok(&probe(&ground, super::HOOKS)), "laid");
    let aterm = scratch.home.join(".aterm");
    std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o775)).unwrap();
    let Probe::Change { from, .. } = probe(&ground, super::HOOKS) else {
        panic!("a group-writable ~/.aterm is a change");
    };
    assert!(
        from.contains("not your own private (0700) directory"),
        "{from}"
    );
    let rows = run(&ground, &[super::HOOKS], Mode::Apply);
    assert_eq!(
        row(&rows, super::HOOKS).outcome,
        Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        std::fs::metadata(&aterm).unwrap().mode() & 0o7777,
        0o700,
        "read back"
    );

    let scratch = Scratch::new("linked-dir");
    let zshrc = scratch.home.join(".zshrc");
    std::fs::write(&zshrc, "export EDITOR=vi\n").unwrap();
    let ground = scratch.ground(Pass::Doctor, None);
    run(&ground, &[super::HOOKS], Mode::Apply);
    let shell_d = scratch.home.join(".aterm/shell.d");
    let elsewhere = scratch.home.join("elsewhere");
    std::fs::rename(&shell_d, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &shell_d).unwrap();
    assert!(is_change(&probe(&ground, super::HOOKS)), "a linked shell.d");
    let rows = run(&ground, &[super::HOOKS, super::RC], Mode::Apply);
    assert_eq!(
        row(&rows, super::HOOKS).outcome,
        Outcome::Failed,
        "{rows:?}"
    );
    assert_eq!(row(&rows, super::RC).outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(
        std::fs::read_to_string(&zshrc).unwrap(),
        "export EDITOR=vi\n",
        "no rc wired to a directory that is not the account's own"
    );
}

#[test]
fn one_unreadable_dev_link_marker_holds_no_other_programs_commands_back() {
    let scratch = Scratch::new("bad-marker");
    install(&scratch.layout, "ay", 17);
    install(&scratch.layout, "az", 3);
    std::fs::create_dir_all(scratch.layout.links_dir()).unwrap();
    std::fs::write(scratch.layout.link_marker("az"), b"not a marker {").unwrap();
    let ay = crate::store::ToolName::new("ay").unwrap();
    std::fs::remove_file(scratch.layout.shim(&ay)).unwrap();
    let doctor = scratch.ground(Pass::Doctor, None);
    let Probe::Change { from, .. } = probe(&doctor, super::SHIMS) else {
        panic!("ay's deleted shim is a change whatever az's marker says");
    };
    assert!(from.contains("ay") && from.contains("az"), "{from}");
    let rows = run(&doctor, &[super::SHIMS], Mode::Apply);
    let shims = row(&rows, super::SHIMS);
    assert_eq!(shims.outcome, Outcome::Failed, "{rows:?}");
    assert!(
        shims.why.as_deref().is_some_and(|why| why.contains("az")),
        "{shims:?}"
    );
    assert!(
        crate::platform::resolve_shim(&scratch.layout.shim(&ay)).is_some(),
        "ay is restored"
    );

    assert!(
        matches!(probe(&doctor, super::SHIMS), Probe::Blocked { .. }),
        "nothing broken: the unreadable marker is named"
    );
    let repair = scratch.ground(Pass::Repair, None);
    assert!(
        is_change(&probe(&repair, super::SHIMS)),
        "repair re-lays the rest"
    );
}

#[test]
fn an_unreadable_exec_root_is_attempted_by_repair_and_unknown_to_the_doctor() {
    let scratch = Scratch::new("unread-root");
    let dir = trust_needing_a_root(&scratch.layout);
    let rustc = dir.join("bin/rustc");
    std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = std::fs::read(&rustc).is_err();
    let doctor = scratch.ground(Pass::Doctor, None);
    let repair = scratch.ground(Pass::Repair, None);
    let doctor_rows = run(&doctor, &[super::EXEC_ROOTS], Mode::Apply);
    let repair_rows = run(&repair, &[super::EXEC_ROOTS], Mode::Apply);
    std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o644)).unwrap();
    if !unreadable {
        eprintln!("NOT RUN: a mode-000 file is readable to this uid");
        return;
    }
    assert_eq!(
        row(&doctor_rows, super::EXEC_ROOTS).outcome,
        Outcome::Unknown,
        "{doctor_rows:?}"
    );
    let attempted = row(&repair_rows, super::EXEC_ROOTS);
    assert_eq!(attempted.outcome, Outcome::Failed, "{repair_rows:?}");
    assert!(
        attempted
            .why
            .as_deref()
            .is_some_and(|why| why.contains("8595")),
        "{attempted:?}"
    );
}

#[test]
fn a_repair_re_lay_over_an_ok_probe_is_journaled() {
    let scratch = Scratch::new("relay-journal");
    install(&scratch.layout, "ay", 17);
    let state = crate::testdir::dir("atpkg-repair-journal-");
    for pass in [Pass::Doctor, Pass::Repair] {
        let items: Vec<Box<dyn aterm_converge::Item>> = super::items(&scratch.ground(pass, None))
            .into_iter()
            .filter(|item| item.id() == super::SHIMS)
            .collect();
        let mut journal = aterm_converge::FileJournal::under(&state);
        let rows = aterm_converge::run_scoped(
            &items,
            Mode::Apply,
            Scope::REPAIR,
            &mut journal,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(row(&rows, super::SHIMS).outcome, Outcome::Ok, "{rows:?}");
        let lines = std::fs::read_to_string(journal.path()).unwrap_or_default();
        match pass {
            Pass::Doctor => assert!(lines.is_empty(), "{lines}"),
            Pass::Repair => assert!(lines.contains(super::SHIMS), "{lines}"),
        }
    }
}

#[test]
fn outside_the_accounts_own_shell_repair_changes_nothing() {
    let scratch = Scratch::new("not-own");
    install(&scratch.layout, "ay", 17);
    let ay = crate::store::ToolName::new("ay").unwrap();
    std::fs::remove_file(scratch.layout.shim(&ay)).unwrap();
    let items = super::items(&scratch.ground(Pass::Repair, None));
    let scope = Scope {
        not_own_account: Some("it runs as root"),
        ..Scope::REPAIR
    };
    let rows = aterm_converge::run_scoped(
        &items,
        Mode::Apply,
        scope,
        &mut aterm_converge::ReadOnly,
        &mut |_| {},
    )
    .unwrap();
    let shims = row(&rows, super::SHIMS);
    assert_eq!(shims.outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(
        shims.why.as_deref(),
        Some(aterm_converge::not_own_account("it runs as root").as_str())
    );
    assert!(
        std::fs::symlink_metadata(scratch.layout.shim(&ay)).is_err(),
        "nothing restored"
    );
    assert!(
        !scratch.home.join(".aterm").exists(),
        "no hook laid in the person's tree"
    );
}

#[test]
fn an_rc_no_pass_edits_is_said_left_alone() {
    let scratch = Scratch::new("rc-dangling");
    std::os::unix::fs::symlink(scratch.home.join("gone"), scratch.home.join(".zshrc")).unwrap();
    let ground = scratch.ground(Pass::Repair, None);
    let Probe::Ok { now } = probe(&ground, super::RC) else {
        panic!("nothing to wire, and the rc that stands is said");
    };
    assert!(
        now.contains("~/.zshrc left alone — atpkg edits only a regular file"),
        "{now}"
    );
    assert_ne!(now, super::RC_ALL_WIRED);
}

#[test]
fn a_relay_says_the_exec_root_it_laid_in_its_row() {
    let scratch = Scratch::new("relay-says");
    trust_needing_a_root(&scratch.layout);
    break_the_tippy_shim(&scratch.layout);
    let ground = scratch.ground(Pass::Doctor, None);
    let rows = run(&ground, &[super::SHIMS], Mode::Apply);
    let shims = row(&rows, super::SHIMS);
    assert_eq!(shims.outcome, Outcome::Changed, "{rows:?}");
    assert!(
        shims
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("tippy now runs from")),
        "{shims:?}"
    );
}

const RELAY_PROBE_ENV: &str = "ATPKG_RELAY_PROBE_PREFIX";
const RELAY_PROBE: &str = "converge::repair::tests::relay_prints_nothing_probe";

#[test]
fn relay_prints_nothing_probe() {
    let Some(prefix) = std::env::var_os(RELAY_PROBE_ENV) else {
        return;
    };
    let relaid = super::relay(&Layout {
        prefix: PathBuf::from(prefix),
    });
    assert!(relaid.failed.is_empty(), "{:?}", relaid.failed);
}

#[test]
fn a_relay_writes_nothing_to_stdout() {
    let scratch = Scratch::new("relay-quiet");
    trust_needing_a_root(&scratch.layout);
    break_the_tippy_shim(&scratch.layout);
    let exe = std::env::current_exe().unwrap();
    let mut child = aterm_uds::spawnfd::command(&exe);
    child
        .args(["--exact", "--nocapture", RELAY_PROBE])
        .env(RELAY_PROBE_ENV, &scratch.layout.prefix)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let out = child.output().expect("run the probe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the probe ran: {out:?}"
    );
    assert!(
        crate::compat::root_dir(&scratch.layout, 8595)
            .join("bin/rustc")
            .is_file(),
        "the re-lay laid the exec root"
    );
    assert!(!stdout.contains("now runs from"), "{stdout}");
}

fn break_the_tippy_shim(layout: &Layout) {
    let shim = layout.bin_dir().join("tippy");
    std::fs::write(
        &shim,
        crate::platform::sh_shim_content_env(
            &layout.build_dir("trust", 8595).join("bin/tippy-gone"),
            &crate::shim_env::ShimEnv::NONE,
        ),
    )
    .unwrap();
}
