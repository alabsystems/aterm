// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::fs::PermissionsExt as _;

use super::*;

struct Fixture {
    _temp: aterm_tempfile::TempDir,
    layout: Layout,
    target: PathBuf,
    entry: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = aterm_tempfile::Builder::new()
            .prefix("managed-launch-")
            .tempdir()
            .unwrap();
        let layout = Layout {
            prefix: temp.path().canonicalize().unwrap(),
        };
        let target = package(&layout.build_dir("codex", 2_026_101_601), "A");
        select(&layout, &target);
        let entry = crate::activate::exec_dir(&layout, "codex").join("codex");
        Self {
            _temp: temp,
            layout,
            target,
            entry,
        }
    }

    fn hints(&self) -> Hints {
        Hints::capture(
            &self.layout,
            &ProcArgs {
                exec_path: self.target.to_string_lossy().into_owned(),
                argv: vec![self.target.to_string_lossy().into_owned()],
                env: vec![
                    "ATPKG_MANAGED_LAUNCH_PID=4242".into(),
                    format!("ATPKG_MANAGED_LAUNCH_PATH={}", self.entry.display()),
                    format!("ATPKG_MANAGED_LAUNCH_TARGET={}", self.target.display()),
                ],
            },
        )
        .unwrap()
    }
}

fn package(root: &Path, label: &str) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let target = root.join("bin/codex");
    std::fs::write(
        &target,
        format!("#!/bin/sh\nprintf '%s\\n' '{label}' \"$0\" \"$@\" \"${{CODEX_HOME-}}\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        root.join(crate::alab::PACKAGE_JSON),
        r#"{"layoutVersion":1,"entrypoint":"bin/codex","version":"fixture"}"#,
    )
    .unwrap();
    std::fs::write(root.join(crate::alab::ALAB_BUILD_JSON), "{}").unwrap();
    crate::store::mark_build_ready(root).unwrap();
    target
}

fn select(layout: &Layout, target: &Path) {
    let root = target.parent().unwrap().parent().unwrap();
    crate::activate::activate_build(layout, root).unwrap();
    crate::activate::install_shims(layout, root, &["codex".into()], crate::Aliases::Off).unwrap();
}

fn run(path: &Path, args: &[&str]) -> Vec<String> {
    let mut command = aterm_uds::spawnfd::command(path);
    command.args(args).env("CODEX_HOME", "/private/saved home");
    let output = aterm_uds::deadline::output_until(
        &mut command,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn raw_exec_admission_binds_pid_image_package_and_the_entire_current_producer() {
    let fixture = Fixture::new();
    let hints = fixture.hints();
    std::fs::remove_file(&fixture.entry).unwrap();
    assert!(hints.admits(4242, &fixture.target, &fixture.entry));
    assert!(!fixture.entry.exists(), "admission is read-only");
    assert!(!hints.admits(4243, &fixture.target, &fixture.entry));
    assert!(!hints.admits(4242, &fixture.target, &fixture.layout.prefix.join("other")));
    let foreign = package(
        &fixture.layout.prefix.join("foreign/store/codex/2026101601"),
        "foreign",
    );
    assert!(!hints.admits(4242, &foreign, &fixture.entry));
    let mut forged = hints.clone();
    forged.target = foreign;
    assert!(!forged.admits(4242, &forged.target, &fixture.entry));
    for pid in ["04242", "+4242", "4242 "] {
        let mut forged = hints.clone();
        forged.pid = pid.into();
        assert!(!forged.admits(4242, &fixture.target, &fixture.entry));
    }
    let primary = fixture.layout.bin_dir().join("codex");
    let original = std::fs::read_to_string(&primary).unwrap();
    for body in [
        format!("#!/bin/sh\nexec '/foreign' \"$@\"\n{original}"),
        original.replace(
            "export ATPKG_MANAGED_LAUNCH_PID=\"$$\"",
            "exit 0\nexport ATPKG_MANAGED_LAUNCH_PID=\"$$\"",
        ),
    ] {
        std::fs::write(&primary, body).unwrap();
        assert!(!hints.admits(4242, &fixture.target, &fixture.entry));
        assert!(prepare_resume(&fixture.layout, &fixture.entry).is_err());
        assert!(!fixture.entry.exists());
    }
    std::fs::write(primary, original).unwrap();
    std::fs::write(
        fixture
            .target
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(crate::alab::ALAB_BUILD_JSON),
        "[]",
    )
    .unwrap();
    assert!(!hints.admits(4242, &fixture.target, &fixture.entry));
}

#[test]
fn native_default_producer_admission_survives_a_different_inspecting_image() {
    let fixture = Fixture::new();
    let helper = fixture.layout.prefix.join("app/aterm");
    std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
    std::fs::write(&helper, b"inert producer image").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let prelude = crate::stub::with_current_exe(&helper, || {
        crate::platform::twin_prelude(
            "codex",
            &fixture.layout.prefix,
            Some(&helper),
            crate::selfupdate::verbs_of("codex"),
            &crate::yolo::policy_of("codex"),
        )
    });
    let body = crate::platform::sh_shim_content_managed(
        &fixture.target,
        &crate::shim_env::ShimEnv::NONE,
        &fixture.entry,
        &prelude,
    );
    assert!(body.contains(crate::native_defaults::VERB));
    std::fs::write(fixture.layout.bin_dir().join("codex"), &body).unwrap();
    std::fs::remove_file(&fixture.entry).unwrap();
    assert!(
        fixture
            .hints()
            .admits(4242, &fixture.target, &fixture.entry)
    );
    prepare_resume(&fixture.layout, &fixture.entry).unwrap();
    assert_eq!(std::fs::read_link(&fixture.entry).unwrap(), fixture.target);
    std::fs::write(
        fixture.layout.bin_dir().join("codex"),
        format!("{body}printf injected\\n"),
    )
    .unwrap();
    assert!(
        !fixture
            .hints()
            .admits(4242, &fixture.target, &fixture.entry)
    );
}

#[test]
fn a_missing_resume_entry_is_repaired_to_current_b_without_rolling_back_raw_a() {
    let fixture = Fixture::new();
    let hints = fixture.hints();
    std::fs::remove_file(&fixture.entry).unwrap();
    let args = ["--search", "-s", "read-only", "resume", "saved-thread"];
    let raw = run(&fixture.layout.bin_dir().join("codex"), &args);
    assert_eq!(&raw[..2], &["A", fixture.target.to_str().unwrap()]);
    assert_eq!(&raw[2..7], args);
    let new = package(&fixture.layout.build_dir("codex", 2_026_101_602), "B");
    select(&fixture.layout, &new);
    std::fs::remove_file(&fixture.entry).unwrap();
    assert!(hints.admits(4242, &fixture.target, &fixture.entry));
    prepare_resume(&fixture.layout, &fixture.entry).unwrap();
    assert_eq!(std::fs::read_link(&fixture.entry).unwrap(), new);
    let resumed = run(&fixture.entry, &args);
    assert_eq!(&resumed[..2], &["B", fixture.entry.to_str().unwrap()]);
    assert_eq!(&resumed[2..7], args);
    assert_eq!(resumed[7], "/private/saved home");
    prepare_resume(&fixture.layout, &fixture.entry).unwrap();
    let before = std::fs::read_link(&fixture.entry).unwrap();
    prepare_resume(&fixture.layout, &fixture.target).unwrap();
    assert_eq!(std::fs::read_link(&fixture.entry).unwrap(), before);
}

#[test]
fn resume_repair_refuses_obstructions_foreign_producers_and_busy_writers() {
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.entry).unwrap();
    std::fs::write(&fixture.entry, "keep this obstruction").unwrap();
    assert!(prepare_resume(&fixture.layout, &fixture.entry).is_err());
    assert_eq!(
        std::fs::read(&fixture.entry).unwrap(),
        b"keep this obstruction"
    );
    std::fs::remove_file(&fixture.entry).unwrap();
    std::os::unix::fs::symlink(&fixture.target, &fixture.entry).unwrap();
    let foreign = package(&fixture.layout.prefix.join("foreign"), "foreign");
    std::fs::remove_file(&fixture.entry).unwrap();
    std::os::unix::fs::symlink(&foreign, &fixture.entry).unwrap();
    assert!(prepare_resume(&fixture.layout, &fixture.entry).is_err());
    assert_eq!(std::fs::read_link(&fixture.entry).unwrap(), foreign);
    std::fs::remove_file(&fixture.entry).unwrap();
    let parent = fixture.entry.parent().unwrap();
    let saved = parent.with_file_name("saved-exec");
    std::fs::rename(parent, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, parent).unwrap();
    assert!(prepare_resume(&fixture.layout, &fixture.entry).is_err());
    assert!(
        std::fs::symlink_metadata(parent)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!saved.join("codex").exists());
    std::fs::remove_file(parent).unwrap();
    std::fs::rename(saved, parent).unwrap();
    let lock = crate::lock::try_lock_store(&fixture.layout).unwrap();
    assert_eq!(
        prepare_resume(&fixture.layout, &fixture.entry)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(lock);
    let primary = fixture.layout.bin_dir().join("codex");
    let foreign_body = crate::platform::sh_shim_content_managed(
        &foreign,
        &crate::shim_env::ShimEnv::NONE,
        &fixture.entry,
        "",
    );
    std::fs::write(&primary, foreign_body).unwrap();
    assert!(prepare_resume(&fixture.layout, &fixture.entry).is_err());
    assert!(!fixture.entry.exists());
}

#[test]
fn a_real_dev_link_is_admitted_and_duplicate_exec_hints_are_not() {
    let fixture = Fixture::new();
    let checkout = fixture.layout.prefix.join("checkout");
    let target = package(&checkout, "dev");
    crate::linkmode::link(&fixture.layout, "codex", &checkout, &["bin/codex".into()]).unwrap();
    let hints = Hints {
        prefix: fixture.layout.prefix.clone(),
        pid: "4242".into(),
        entry: fixture.entry.clone(),
        target: target.clone(),
        legacy_store: false,
    };
    std::fs::remove_file(&fixture.entry).unwrap();
    assert!(hints.admits(4242, &target, &fixture.entry));
    let next_checkout = fixture.layout.prefix.join("next-checkout");
    let next = package(&next_checkout, "next-dev");
    crate::linkmode::link(
        &fixture.layout,
        "codex",
        &next_checkout,
        &["bin/codex".into()],
    )
    .unwrap();
    std::fs::remove_file(&fixture.entry).unwrap();
    assert!(
        hints.admits(4242, &target, &fixture.entry),
        "old dev A retains route authority after current producer B is published"
    );
    prepare_resume(&fixture.layout, &fixture.entry).unwrap();
    assert_eq!(std::fs::read_link(&fixture.entry).unwrap(), next);
    let mut args = ProcArgs {
        exec_path: String::new(),
        argv: vec![],
        env: vec![
            "ATPKG_MANAGED_LAUNCH_PID=4242".into(),
            format!("ATPKG_MANAGED_LAUNCH_PATH={}", fixture.entry.display()),
            format!("ATPKG_MANAGED_LAUNCH_TARGET={}", target.display()),
        ],
    };
    assert!(Hints::capture(&fixture.layout, &args).is_some());
    args.env.push("ATPKG_MANAGED_LAUNCH_TARGET".into());
    assert!(Hints::capture_for_pid(&fixture.layout, &args, 4242).is_none());
    args.env.pop();
    args.env.push("ATPKG_MANAGED_LAUNCH_PID=4242".into());
    assert!(Hints::capture(&fixture.layout, &args).is_none());
}

#[test]
fn legacy_kernel_exec_requires_exact_store_image_and_independent_current_producer() {
    let fixture = Fixture::new();
    let args = ProcArgs {
        exec_path: fixture.target.to_string_lossy().into_owned(),
        argv: vec![
            fixture.entry.to_string_lossy().into_owned(),
            "resume".into(),
        ],
        env: vec![],
    };
    let hints = Hints::capture_for_pid(&fixture.layout, &args, 4242).unwrap();
    let newer = package(&fixture.layout.build_dir("codex", 2_026_101_602), "B");
    select(&fixture.layout, &newer);
    assert!(hints.admits(4242, &fixture.target, &fixture.entry));
    assert!(!hints.admits(4243, &fixture.target, &fixture.entry));
    assert!(!hints.admits(4242, &newer, &fixture.entry));
    assert!(!hints.admits(4242, &fixture.target, &fixture.target));
    for pid in [0, 1] {
        assert!(Hints::capture_for_pid(&fixture.layout, &args, pid).is_none());
    }
    for argv0 in [
        fixture.target.clone(),
        fixture.layout.prefix.join("exec/codex"),
    ] {
        let mut wrong = args.clone();
        wrong.argv[0] = argv0.to_string_lossy().into_owned();
        assert!(Hints::capture_for_pid(&fixture.layout, &wrong, 4242).is_none());
    }
    let mut alias = args.clone();
    alias.exec_path = format!(
        "{}/../bin/codex",
        fixture.target.parent().unwrap().display()
    );
    assert!(Hints::capture_for_pid(&fixture.layout, &alias, 4242).is_none());
    let alias_dir = fixture.layout.prefix.join("alias");
    std::os::unix::fs::symlink(fixture.target.parent().unwrap(), &alias_dir).unwrap();
    alias.exec_path = alias_dir.join("codex").to_string_lossy().into_owned();
    assert!(Hints::capture_for_pid(&fixture.layout, &alias, 4242).is_none());
    for target in [
        package(
            &fixture.layout.prefix.join("foreign/store/codex/2026101601"),
            "F",
        ),
        package(&fixture.layout.prefix.join("retained-dev"), "D"),
    ] {
        let mut wrong = args.clone();
        wrong.exec_path = target.to_string_lossy().into_owned();
        assert!(Hints::capture_for_pid(&fixture.layout, &wrong, 4242).is_none());
    }
    let root = fixture.target.parent().unwrap().parent().unwrap();
    let marker = crate::store::ready_marker_path(root).unwrap();
    let ready = std::fs::read(&marker).unwrap();
    std::fs::remove_file(&marker).unwrap();
    assert!(Hints::capture_for_pid(&fixture.layout, &args, 4242).is_none());
    assert!(!hints.admits(4242, &fixture.target, &fixture.entry));
    std::fs::write(&marker, ready).unwrap();
    let shim = fixture.layout.bin_dir().join("codex");
    let old = std::fs::read(&shim).unwrap();
    std::fs::write(&shim, b"#!/bin/sh\nexit 0\n").unwrap();
    assert!(!hints.admits(4242, &fixture.target, &fixture.entry));
    std::fs::write(&shim, old).unwrap();
    std::fs::write(
        fixture
            .target
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(crate::alab::ALAB_BUILD_JSON),
        "not-json",
    )
    .unwrap();
    assert!(!hints.admits(4242, &fixture.target, &fixture.entry));
}

#[test]
fn legacy_admission_never_recovers_partial_or_malformed_modern_hints() {
    let fixture = Fixture::new();
    let mut args = ProcArgs {
        exec_path: fixture.target.to_string_lossy().into_owned(),
        argv: vec![fixture.entry.to_string_lossy().into_owned()],
        env: vec![],
    };
    for field in [
        "ATPKG_MANAGED_LAUNCH_PID",
        "ATPKG_MANAGED_LAUNCH_PID=",
        "ATPKG_MANAGED_LAUNCH_PID=4242",
        "ATPKG_MANAGED_LAUNCH_PATH=",
        "ATPKG_MANAGED_LAUNCH_TARGET=",
    ] {
        args.env = vec![field.into()];
        assert!(
            Hints::capture_for_pid(&fixture.layout, &args, 4242).is_none(),
            "{field}"
        );
    }
    args.env = vec![
        "ATPKG_MANAGED_LAUNCH_PID=04242".into(),
        format!("ATPKG_MANAGED_LAUNCH_PATH={}", fixture.entry.display()),
        format!("ATPKG_MANAGED_LAUNCH_TARGET={}", fixture.target.display()),
    ];
    let malformed = Hints::capture_for_pid(&fixture.layout, &args, 4242).unwrap();
    assert!(!malformed.admits(4242, &fixture.target, &fixture.entry));
    args.env.push("ATPKG_MANAGED_LAUNCH_TARGET".into());
    assert!(Hints::capture_for_pid(&fixture.layout, &args, 4242).is_none());
    args.env.pop();
    args.env.push("ATPKG_MANAGED_LAUNCH_PID=4242".into());
    assert!(Hints::capture_for_pid(&fixture.layout, &args, 4242).is_none());
}
