// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn real_directory_check_refuses_files_missing_paths_and_directory_aliases() {
    let world = aterm_tempfile::tempdir().unwrap();
    let real = world.path().join("real");
    let file = world.path().join("file");
    let alias = world.path().join("alias");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(&file, b"preserved").unwrap();
    std::fs::write(real.join("kept"), b"target contents").unwrap();
    assert!(is_real_dir(&real));
    assert!(!is_real_dir(&file));
    assert!(!is_real_dir(&world.path().join("missing")));
    crate::platform::atomic_symlink(&real, &alias).unwrap();
    assert!(!is_real_dir(&alias));
    crate::platform::remove_link(&alias);
    assert!(std::fs::symlink_metadata(&alias).is_err());
    assert_eq!(
        std::fs::read(real.join("kept")).unwrap(),
        b"target contents"
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"preserved");
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "the tree's drop guard; only the unix tests read it"
        )
    )]
    root: aterm_tempfile::TempDir,
    layout: Layout,
    rustup: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-seam-{label}-"))
            .tempdir()
            .unwrap();
        let prefix = root.path().join("prefix");
        std::fs::create_dir_all(&prefix).unwrap();
        let rustup = root.path().join("rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
        Fixture {
            root,
            layout: Layout { prefix },
            rustup,
        }
    }

    fn install_trust(&self, build: u64) -> PathBuf {
        let dir = self.layout.build_dir("trust", build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("lib").join("rustlib")).unwrap();
        std::fs::write(
            dir.join("lib").join("libtrust.dylib"),
            format!("driver of {build}"),
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            "../../etc",
            dir.join("lib").join("rustlib").join("etc-link"),
        )
        .unwrap();
        for tool in ["trustc", "targo", "trustdoc", "tippy"] {
            std::fs::write(
                dir.join("bin").join(tool),
                format!("{tool} of build {build}"),
            )
            .unwrap();
        }
        crate::activate::atomic_symlink(&dir, &store_current(&self.layout)).unwrap();
        dir
    }

    fn seam(&self, name: &str) -> PathBuf {
        seam_path(&self.rustup, name)
    }

    fn seams_recorded(&self) -> Vec<String> {
        recorded_keys(&self.layout)
    }
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// The refusal an event carries.
#[cfg(unix)]
fn refusal(e: &SeamEvent) -> Option<&Refusal> {
    match e {
        SeamEvent::Refused { refusal, .. } => Some(refusal),
        _ => None,
    }
}

/// The attach an event carries.
#[cfg(unix)]
fn attached(e: &SeamEvent) -> Option<&Attached> {
    match e {
        SeamEvent::Attached(a) => Some(a),
        _ => None,
    }
}

/// A foreign entry left alone, its one fix [`DETACH_FIX`].
#[cfg(unix)]
fn is_foreign(e: &SeamEvent) -> bool {
    matches!(refusal(e), Some(Refusal::Foreign { .. }))
}

/// A link put back after a heal, left as it stands ([`Refusal::PutBack`]).
#[cfg(unix)]
fn is_put_back(e: &SeamEvent) -> bool {
    matches!(refusal(e), Some(Refusal::PutBack { .. }))
}

/// The link an older toolchain's replacement re-pointed away from — the one whose way back the
/// line prints.
#[cfg(unix)]
fn replaced_older(e: &SeamEvent) -> Option<&Path> {
    match attached(e) {
        Some(Attached::ReplacedStale {
            from,
            stale: Stale::Older { .. },
            ..
        }) => Some(from),
        _ => None,
    }
}

#[cfg(unix)]
fn cloned(store: &Path, at: &Path) -> bool {
    crate::clone::is_clone_of(store, at) && matches!(crate::clone::same_bytes(store, at), Ok(true))
}

#[test]
fn allowlist_and_record_key() {
    assert!(name_allowed("trust"));
    assert!(name_allowed("trust-dev"));
    assert!(!name_allowed("stable"));
    assert!(!name_allowed(""));
    assert!(!name_allowed("../trust"));
    assert_eq!(record_key("trust"), "rustup:trust");
}

#[test]
fn rustup_home_prefers_env_then_home() {
    let env = std::ffi::OsString::from("/x/rh");
    assert_eq!(
        rustup_home_with(Some(&env), Some(Path::new("/h"))),
        Some(PathBuf::from("/x/rh"))
    );
    let empty = std::ffi::OsString::new();
    assert_eq!(
        rustup_home_with(Some(&empty), Some(Path::new("/h"))),
        Some(PathBuf::from("/h/.rustup"))
    );
    assert_eq!(
        rustup_home_with(None, Some(Path::new("/h"))),
        Some(PathBuf::from("/h/.rustup"))
    );
    assert_eq!(rustup_home_with(None, None), None);
    assert_eq!(
        rustup_home_with(Some(&env), None),
        Some(PathBuf::from("/x/rh"))
    );
}

#[test]
fn normalize_folds_dots() {
    assert_eq!(
        normalize(Path::new("/a/b/../c/./d")),
        PathBuf::from("/a/c/d")
    );
    assert_eq!(normalize(Path::new("/a/b")), PathBuf::from("/a/b"));
}

#[test]
fn bad_name_is_refused_before_touching_disk() {
    let fx = Fixture::new("badname");
    fx.install_trust(6808);
    let err = attach(&fx.layout, &fx.rustup, "stable").unwrap_err();
    assert!(matches!(err, Refusal::BadName(_)), "{err}");
    assert!(std::fs::symlink_metadata(fx.seam("stable")).is_err());
    assert!(fx.seams_recorded().is_empty());
    let err = detach(&fx.layout, &fx.rustup, "stable", true).unwrap_err();
    assert!(matches!(err, Refusal::BadName(_)));
}

#[test]
fn no_rustup_is_a_noop_that_never_creates_rustup_home() {
    let fx = Fixture::new("norustup");
    fx.install_trust(6808);
    std::fs::remove_dir_all(fx.rustup.join("toolchains")).unwrap();
    let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(matches!(out, Attached::NoRustup { .. }), "{out}");
    assert!(!out.is_live() && !out.changed());
    assert!(!fx.rustup.join("toolchains").exists(), "never created");
    assert!(fx.seams_recorded().is_empty(), "nothing recorded");
    let s = status(&fx.layout, &fx.rustup, "trust");
    assert!(!s.rustup_present);
    assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    assert!(
        !fx.rustup.join("toolchains").exists(),
        "reassert never creates it"
    );
}

#[cfg(unix)]
#[test]
fn absent_is_created_targeting_current_and_recorded() {
    let fx = Fixture::new("absent");
    fx.install_trust(6808);
    let before = status(&fx.layout, &fx.rustup, "trust");
    assert!(!before.recorded && before.entry == Ok(Entry::Absent));

    let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(matches!(out, Attached::Created { .. }), "{out}");
    assert!(out.changed());
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust"),
        "the link names the view, never the store"
    );
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    let leftovers: Vec<_> = std::fs::read_dir(toolchains_dir(&fx.rustup))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "no temp symlink remains");

    let after = status(&fx.layout, &fx.rustup, "trust");
    assert!(after.recorded && after.in_prefix && after.targets_view);

    let again = attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(matches!(again, Attached::Adopted { .. }), "{again}");
    assert!(!again.changed());
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
}

#[cfg(unix)]
#[test]
fn attach_refuses_when_trust_is_not_installed() {
    let fx = Fixture::new("notinstalled");
    let err = attach(&fx.layout, &fx.rustup, "trust").unwrap_err();
    assert!(matches!(err, Refusal::NotInstalled { .. }), "{err}");
    assert!(
        std::fs::symlink_metadata(fx.seam("trust")).is_err(),
        "no dangling link laid"
    );
    assert!(fx.seams_recorded().is_empty());
}

/// A view laid from a whole `current`, whose build `lose` then breaks: both doors refuse
/// to lay from it and record why, and the view keeps its clones, which still run. A whole
/// `current` ends the refusal.
#[cfg(unix)]
fn a_broken_build_keeps_the_view(label: &str, lacks: BuildLack, lose: impl FnOnce(&Path)) {
    let fx = Fixture::new(label);
    let build = fx.install_trust(9192);
    let _ = reassert(&fx.layout, &fx.rustup);
    let view = seam_target(&fx.layout, "trust");
    let driver = || std::fs::read_to_string(view.join("lib").join("libtrust.dylib")).ok();
    assert_eq!(driver().as_deref(), Some("driver of 9192"), "PRECONDITION");

    lose(&build);
    for door in [reassert, repair] {
        let events = door(&fx.layout, &fx.rustup);
        assert_eq!(driver().as_deref(), Some("driver of 9192"), "{events:?}");
        assert!(view.join("bin").join("trustc").is_file(), "{events:?}");
        let refused = refusals(&fx.layout);
        assert!(
            refused.len() == 1 && refused[0].1.contains(&build.display().to_string()),
            "{refused:?} {events:?}"
        );
        assert!(
            events.len() == 1
                && matches!(
                    refusal(&events[0]),
                    Some(Refusal::NoBuild { lacks: said, .. }) if *said == lacks
                ),
            "{events:?}"
        );
    }

    fx.install_trust(9200);
    let _ = reassert(&fx.layout, &fx.rustup);
    assert_eq!(driver().as_deref(), Some("driver of 9200"));
    assert!(refusals(&fx.layout).is_empty());
}

#[cfg(unix)]
#[test]
fn a_removed_build_is_refused_and_the_view_kept() {
    a_broken_build_keeps_the_view("gone", BuildLack::Tree, |build| {
        std::fs::remove_dir_all(build).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn a_build_link_to_a_removed_aside_is_refused_and_the_view_kept() {
    a_broken_build_keeps_the_view("aside", BuildLack::Tree, |build| {
        std::fs::remove_dir_all(build).unwrap();
        crate::store::link_aside(build).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn a_build_link_to_another_tree_is_refused_and_the_view_kept() {
    a_broken_build_keeps_the_view("elsewhere", BuildLack::OwnTree, |build| {
        let elsewhere = build.with_file_name("elsewhere");
        std::fs::rename(build, &elsewhere).unwrap();
        std::fs::write(elsewhere.join("lib").join("libtrust.dylib"), "another tree").unwrap();
        link(&elsewhere, build);
    });
}

#[cfg(unix)]
#[test]
fn a_build_that_lost_only_its_lib_is_refused_and_the_view_kept() {
    a_broken_build_keeps_the_view("nolib", BuildLack::Lib, |build| {
        std::fs::remove_dir_all(build.join("lib")).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn a_build_that_lost_its_bin_is_refused_and_the_view_kept() {
    a_broken_build_keeps_the_view("nobin", BuildLack::Bin, |build| {
        std::fs::remove_dir_all(build.join("bin")).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn a_build_that_lost_its_sysroot_is_refused_and_the_view_kept() {
    a_broken_build_keeps_the_view("norustlib", BuildLack::Lib, |build| {
        std::fs::remove_dir_all(build.join("lib").join("rustlib")).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn a_build_that_lost_a_recorded_tool_is_refused_and_the_view_kept() {
    a_broken_build_keeps_the_view("notool", BuildLack::Bin, |build| {
        std::fs::write(
            build.with_file_name("9192.ready"),
            "ok\ncontents=bin:targo,trustc\n",
        )
        .unwrap();
        std::fs::remove_file(build.join("bin").join("trustc")).unwrap();
    });
}

/// The machine an earlier pass already stripped: nothing of the view is left to keep, and
/// it is still told what its build lacks.
#[cfg(unix)]
#[test]
fn a_view_already_stripped_still_hears_what_its_build_lacks() {
    let fx = Fixture::new("stripped");
    let build = fx.install_trust(9192);
    let _ = reassert(&fx.layout, &fx.rustup);
    std::fs::remove_dir_all(build.join("lib")).unwrap();
    std::fs::remove_dir_all(seam_target(&fx.layout, "trust").join("lib")).unwrap();

    let events = reassert(&fx.layout, &fx.rustup);

    assert!(
        events.len() == 1
            && matches!(
                refusal(&events[0]),
                Some(Refusal::NoBuild {
                    lacks: BuildLack::Lib,
                    ..
                })
            ),
        "{events:?}"
    );
    assert_eq!(refusals(&fx.layout).len(), 1);
}

/// `refresh_view` guards itself: the record's doors stop first, and only this stops a
/// caller that reaches it another way.
#[cfg(unix)]
#[test]
fn a_refresh_asked_directly_keeps_the_view_of_a_broken_build() {
    let fx = Fixture::new("direct");
    let build = fx.install_trust(9192);
    refresh_view(&fx.layout, "trust").unwrap();
    std::fs::remove_dir_all(build.join("lib")).unwrap();

    let err = refresh_view(&fx.layout, "trust").unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    let view = seam_target(&fx.layout, "trust");
    assert!(view.join("lib").join("libtrust.dylib").is_file());
}

#[cfg(unix)]
#[test]
fn existing_link_into_prefix_is_adopted_untouched() {
    let fx = Fixture::new("adopt");
    fx.install_trust(6808);
    refresh_view(&fx.layout, "trust").unwrap();
    link(&seam_target(&fx.layout, "trust"), &fx.seam("trust"));
    let ino_before = std::fs::symlink_metadata(fx.seam("trust")).unwrap();
    let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(matches!(out, Attached::Adopted { .. }), "{out}");
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust")
    );
    use std::os::unix::fs::MetadataExt;
    let ino_after = std::fs::symlink_metadata(fx.seam("trust")).unwrap();
    assert_eq!(ino_before.ino(), ino_after.ino(), "adopt rewrote the link");
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
}

#[cfg(unix)]
#[test]
fn relative_link_into_prefix_is_adopted() {
    let fx = Fixture::new("relative");
    fx.install_trust(6808);
    refresh_view(&fx.layout, "trust").unwrap();
    link(Path::new("../../prefix/rustup/trust"), &fx.seam("trust"));
    let p = probe(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(p.in_prefix && p.targets_view, "{p:?}");
    let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(matches!(out, Attached::Adopted { .. }), "{out}");
}

/// A rustup `trust` entry that is not atpkg's is refused with the one fix and left alone.
#[cfg(unix)]
#[test]
fn a_foreign_entry_is_refused_with_the_one_fix_and_left_alone() {
    let fx = Fixture::new("foreign");
    fx.install_trust(6808);
    let elsewhere = fx.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let entry = fx.seam("trust");
    type Lay = fn(&Path, &Path);
    let shapes: [(&str, Lay); 3] = [
        ("a real directory", |entry, _| {
            std::fs::create_dir_all(entry.join("bin")).unwrap();
            std::fs::write(entry.join("bin").join("rustc"), b"x").unwrap();
        }),
        ("a link out of the prefix", |entry, elsewhere| {
            link(elsewhere, entry);
        }),
        ("a regular file", |entry, _| {
            std::fs::write(entry, b"not a toolchain").unwrap();
        }),
    ];
    for (label, lay) in shapes {
        lay(&entry, &elsewhere);
        let before = (
            std::fs::read_link(&entry).ok(),
            std::fs::read(&entry).ok(),
            std::fs::read(entry.join("bin").join("rustc")).ok(),
        );
        let err = attach(&fx.layout, &fx.rustup, "trust").unwrap_err();
        assert!(matches!(err, Refusal::Foreign { .. }), "{label}: {err}");
        assert!(err.to_string().contains(DETACH_FIX), "{label}: {err}");
        assert_eq!(
            (
                std::fs::read_link(&entry).ok(),
                std::fs::read(&entry).ok(),
                std::fs::read(entry.join("bin").join("rustc")).ok(),
            ),
            before,
            "{label}: untouched"
        );
        assert!(fx.seams_recorded().is_empty(), "{label}");
        let s = status(&fx.layout, &fx.rustup, "trust");
        assert!(!s.in_prefix && !s.targets_view, "{label}: {s}");
        if std::fs::symlink_metadata(&entry).unwrap().is_dir() {
            std::fs::remove_dir_all(&entry).unwrap();
        } else {
            std::fs::remove_file(&entry).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn a_refused_reassertion_is_recorded_until_one_succeeds() {
    let fx = Fixture::new("refusal-record");
    fx.install_trust(6808);
    let elsewhere = fx.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    link(&elsewhere, &fx.seam("trust"));
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(is_foreign(&lines[0]), "{lines:?}");
    let refused = refusals(&fx.layout);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "trust");
    assert_eq!(
        fx.seams_recorded(),
        vec![format!("refused:rustup:trust: {}", refused[0].1)]
    );
    assert!(recorded_names(&fx.layout).is_empty());
    let _ = reassert(&fx.layout, &fx.rustup);
    assert_eq!(fx.seams_recorded().len(), 1);
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), elsewhere);
    std::fs::remove_file(fx.seam("trust")).unwrap();
    refresh_view(&fx.layout, "trust").unwrap();
    link(&seam_target(&fx.layout, "trust"), &fx.seam("trust"));
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(lines.is_empty(), "an adoption is quiet: {lines:?}");
    assert!(refusals(&fx.layout).is_empty());
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    assert_eq!(refused_key("trust"), "refused:rustup:trust");
}

/// The unattended pass names the foreign entry it left alone ([`SeamEvent::foreign_entry`]).
#[cfg(unix)]
#[test]
fn the_unattended_pass_names_the_foreign_entry_it_left_alone() {
    let fx = Fixture::new("foreignnamed");
    fx.install_trust(6808);
    let elsewhere = fx.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    link(&elsewhere, &fx.seam("trust"));
    let said = reassert(&fx.layout, &fx.rustup);
    let foreign: Vec<_> = said.iter().filter_map(SeamEvent::foreign_entry).collect();
    assert_eq!(foreign, [("trust", fx.seam("trust").as_path())]);
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), elsewhere);
    std::fs::remove_file(fx.seam("trust")).unwrap();
    let said = reassert(&fx.layout, &fx.rustup);
    assert!(said.iter().all(|e| e.foreign_entry().is_none()), "{said:?}");
}

/// A compiler at `<sysroot>/bin/<name>` answering `-vV` with the `commit-date:` in
/// `<sysroot>/commit-date`.
#[cfg(unix)]
fn dated_compiler(fx: &Fixture, sysroot: &Path, name: &str, date: &str) {
    let script = fx.root.path().join("dated-compiler");
    if !script.is_file() {
        std::fs::write(
            &script,
            "#!/bin/sh\nd=$(cat \"$(dirname \"$0\")/../commit-date\")\n\
             echo \"rustc 1.99.0-dev (0000000 $d)\"\necho \"commit-date: $d\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::process::Command::new(&script).output();
    }
    let bin = sysroot.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(name);
    let _ = std::fs::remove_file(&exe);
    std::fs::hard_link(&script, &exe).unwrap();
    std::fs::write(sysroot.join("commit-date"), date).unwrap();
}

/// An older sealed toolchain (`~/toolchains/trust-<rev>`).
#[cfg(unix)]
#[test]
fn repair_repoints_a_link_to_an_older_or_missing_toolchain_and_nothing_else() {
    let fx = Fixture::new("stale-local");
    let build = fx.install_trust(9192);
    dated_compiler(&fx, &build, "trustc", "2026-09-17");
    let sealed = fx.root.path().join("toolchains").join("trust-0717abcdef");
    dated_compiler(&fx, &sealed, "rustc", "2026-07-17");
    link(&sealed, &fx.seam("trust"));
    assert_eq!(
        stale_against_store(&fx.layout, &sealed),
        Some(Stale::Older {
            its: "2026-07-17".into(),
            store: "2026-09-17".into()
        })
    );
    assert!(!live_build_tree(&sealed));
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(is_foreign(&lines[0]), "{lines:?}");
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), sealed);

    let lines = repair(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        replaced_older(&lines[0]),
        Some(sealed.as_path()),
        "{lines:?}"
    );
    assert!(
        lines[0].to_string().contains(&format!(
            "to put it back: ln -sfn '{}' '{}'",
            sealed.display(),
            fx.seam("trust").display()
        )),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust")
    );
    assert_eq!(
        fx.seams_recorded(),
        vec![
            format!("replaced:rustup:trust: {}", sealed.display()),
            "rustup:trust".to_string()
        ]
    );
    assert!(
        sealed.join("bin").join("rustc").is_file(),
        "the old tree is left"
    );
    assert!(
        repair(&fx.layout, &fx.rustup).is_empty(),
        "then it is ours: adopted"
    );

    for (date, why) in [("2026-09-18", "newer"), ("unknown", "undated")] {
        std::fs::remove_file(fx.seam("trust")).unwrap();
        dated_compiler(&fx, &sealed, "rustc", date);
        link(&sealed, &fx.seam("trust"));
        assert_eq!(stale_against_store(&fx.layout, &sealed), None, "{why}");
        let lines = repair(&fx.layout, &fx.rustup);
        assert!(is_foreign(&lines[0]), "{why}: {lines:?}");
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            sealed,
            "{why}"
        );
    }

    let gone = fx.root.path().join("toolchains").join("trust-deleted");
    std::fs::remove_file(fx.seam("trust")).unwrap();
    link(&gone, &fx.seam("trust"));
    assert!(is_foreign(&reassert(&fx.layout, &fx.rustup)[0]));
    let lines = repair(&fx.layout, &fx.rustup);
    assert!(
        matches!(
            attached(&lines[0]),
            Some(Attached::ReplacedStale { from, stale: Stale::Dangling, .. }) if *from == gone
        ),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust")
    );
    assert_eq!(replaced_from(&fx.layout, "trust"), Some(sealed.clone()));

    let checkout = sysroot_checkout(&fx, "checkout");
    crate::linkmode::link(
        &fx.layout,
        "trust",
        &checkout,
        &[PathBuf::from("bin/trustc")],
    )
    .unwrap();
    dated_compiler(&fx, &sealed, "rustc", "2026-07-17");
    std::fs::remove_file(fx.seam("trust")).unwrap();
    link(&sealed, &fx.seam("trust"));
    assert_eq!(stale_against_store(&fx.layout, &sealed), None);
    assert_eq!(stale_against_store(&fx.layout, &gone), None);
    assert!(is_foreign(&repair(&fx.layout, &fx.rustup)[0]));
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), sealed);
}

/// The store (build 9192, dated 2026-09-17) and a live build tree at `rel` whose `rustc` says
/// `date`.
#[cfg(unix)]
fn stale_tree_fixture(fx: &Fixture, rel: &str, date: &str) -> PathBuf {
    let build = fx.install_trust(9192);
    dated_compiler(fx, &build, "trustc", "2026-09-17");
    let tree = fx.root.path().join(rel);
    dated_compiler(fx, &tree, "rustc", date);
    link(&tree, &fx.seam("trust"));
    tree
}

/// [`reassert`] — the unattended pass, `repair` off — with the process table injected.
#[cfg(unix)]
fn unattended(fx: &Fixture, running: Option<Vec<PathBuf>>) -> Vec<SeamEvent> {
    reassert_with(
        &fx.layout,
        &fx.rustup,
        &refresh_view,
        &|| running.clone(),
        false,
    )
}

/// The shape the unattended heal is limited to: `…/build/<triple|host>/stage[0-2]`.
#[cfg(unix)]
#[test]
fn live_build_tree_is_the_stage_sysroot_shape_and_nothing_near_it() {
    for yes in [
        "/h/trust/build/host/stage2",
        "/h/trust/build/aarch64-apple-darwin/stage2",
        "/w/build/x86_64-unknown-linux-gnu/stage1",
        "/w/build/thumbv7em-none-eabihf/stage0",
        "/w/build/host/stage2/",
        "/w/build/host/stage2/../stage2",
    ] {
        assert!(live_build_tree(Path::new(yes)), "{yes}");
    }
    for no in [
        "/h/toolchains/trust-0820abcdef",
        "/h/toolchains/trust-current",
        "/w/build/host/stage3",
        "/w/build/host/stage2-tools",
        "/w/build/stage2",
        "/w/build/aarch64/stage2",
        "/w/build/aarch64--darwin/stage2",
        "/w/built/host/stage2",
        "/w/build/host/stage2/bin",
        "/w/prefix/store/trust/9100",
        "stage2",
    ] {
        assert!(!live_build_tree(Path::new(no)), "{no}");
    }
    let fx = Fixture::new("tree-shape");
    let tree = fx
        .root
        .path()
        .join("trust/build/aarch64-apple-darwin/stage2");
    std::fs::create_dir_all(&tree).unwrap();
    let alias = fx.root.path().join("toolchains").join("trust-alias");
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    link(&tree, &alias);
    assert!(live_build_tree(&alias));
}

/// The unattended pass re-points an older live build tree at the view.
#[cfg(unix)]
#[test]
fn the_unattended_pass_repoints_an_older_live_build_tree_and_records_the_way_back() {
    for (label, rel) in [
        ("heal-host", "trust/build/host/stage2"),
        ("heal-triple", "trust/build/aarch64-apple-darwin/stage1"),
    ] {
        let fx = Fixture::new(label);
        let tree = stale_tree_fixture(&fx, rel, "2026-08-20");
        let log = fx.root.path().join("logs");
        let _bound = crate::packages_log::test_bind::bind(log.clone(), fx.layout.prefix.clone());
        let lines = unattended(&fx, Some(vec![PathBuf::from("/usr/bin/true")]));
        assert_eq!(lines.len(), 1, "{label}: {lines:?}");
        let logged: Vec<_> = crate::packages_log::read_tail(&log, 10)
            .into_iter()
            .filter(|e| e.kind == crate::packages_log::kind::SEAM)
            .collect();
        assert!(
            logged.len() == 1
                && logged[0].get("seam") == Some("rustup:trust")
                && logged[0].get("from") == Some(tree.to_str().unwrap())
                && logged[0]
                    .get("undo")
                    .is_some_and(|u| u.starts_with("to put it back: ln -sfn")),
            "{label}: {logged:?}"
        );
        assert_eq!(
            replaced_older(&lines[0]),
            Some(tree.as_path()),
            "{label}: {lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            seam_target(&fx.layout, "trust"),
            "{label}"
        );
        assert_eq!(
            fx.seams_recorded(),
            vec![
                format!("replaced:rustup:trust: {}", tree.display()),
                "rustup:trust".to_string()
            ],
            "{label}"
        );
        assert_eq!(replaced_from(&fx.layout, "trust"), Some(tree.clone()));
        assert_eq!(recorded_names(&fx.layout), vec!["trust".to_string()]);
        assert!(
            tree.join("bin").join("rustc").is_file(),
            "{label}: the tree is left"
        );
        let note = replaced_note(&fx.layout, &fx.rustup, "trust").expect("a note");
        assert!(
            note.contains(&format!(
                "ln -sfn '{}' '{}'",
                tree.display(),
                fx.seam("trust").display()
            )),
            "{label}: {note}"
        );
        assert!(unattended(&fx, Some(Vec::new())).is_empty(), "{label}");
        detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
        assert!(fx.seams_recorded().is_empty(), "{label}");
        assert_eq!(replaced_from(&fx.layout, "trust"), None, "{label}");
    }
}

/// The unattended pass leaves every other foreign link.
#[cfg(unix)]
#[test]
fn the_unattended_pass_leaves_every_other_foreign_link() {
    let fx = Fixture::new("heal-not");
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    let refused = |fx: &Fixture, target: &Path, why: &str| {
        let _ = std::fs::remove_file(fx.seam("trust"));
        link(target, &fx.seam("trust"));
        let lines = unattended(fx, Some(Vec::new()));
        assert!(
            lines.len() == 1 && is_foreign(&lines[0]),
            "{why}: {lines:?}"
        );
        assert_eq!(
            std::fs::read_link(fx.seam("trust")).unwrap(),
            target,
            "{why}: nothing moved"
        );
        assert_eq!(replaced_from(&fx.layout, "trust"), None, "{why}");
        assert_eq!(refusals(&fx.layout).len(), 1, "{why}: refused as ever");
    };

    let sealed = fx.root.path().join("toolchains").join("trust-0820abcdef");
    dated_compiler(&fx, &sealed, "rustc", "2026-08-20");
    assert!(matches!(
        stale_against_store(&fx.layout, &sealed),
        Some(Stale::Older { .. })
    ));
    refused(&fx, &sealed, "an older seal");

    let other = fx
        .root
        .path()
        .join("other-prefix")
        .join("store")
        .join("trust")
        .join("9100");
    dated_compiler(&fx, &other, "trustc", "2026-08-20");
    assert!(matches!(
        stale_against_store(&fx.layout, &other),
        Some(Stale::Older { .. })
    ));
    refused(&fx, &other, "another prefix's older store build");

    dated_compiler(&fx, &stage2, "rustc", "2026-09-18");
    refused(&fx, &stage2, "a newer live tree");
    dated_compiler(&fx, &stage2, "rustc", "unknown");
    refused(&fx, &stage2, "a live tree with no date");
    let wedged = stage2.join("bin").join("rustc");
    std::fs::remove_file(&wedged).unwrap();
    std::fs::write(&wedged, "#!/bin/sh\nexec sleep 120\n").unwrap();
    std::fs::set_permissions(&wedged, std::fs::Permissions::from_mode(0o755)).unwrap();
    {
        let _probes = crate::doctor::ShorterProbes::of(std::time::Duration::from_millis(250));
        let started = std::time::Instant::now();
        assert_eq!(stale_against_store(&fx.layout, &stage2), None);
        refused(&fx, &stage2, "a live tree whose driver does not answer");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the probe was cut off by its own bound, not the driver's two minutes: {:?}",
            started.elapsed()
        );
    }
    std::fs::remove_dir_all(&stage2).unwrap();
    std::fs::create_dir_all(&stage2).unwrap();
    refused(&fx, &stage2, "a live tree emptied mid-rebuild");
    std::fs::remove_dir_all(&stage2).unwrap();
    assert_eq!(
        stale_against_store(&fx.layout, &stage2),
        Some(Stale::Dangling)
    );
    refused(&fx, &stage2, "a live tree that is gone");

    dated_compiler(&fx, &stage2, "rustc", "2026-08-20");
    let checkout = sysroot_checkout(&fx, "checkout");
    crate::linkmode::link(
        &fx.layout,
        "trust",
        &checkout,
        &[PathBuf::from("bin/trustc")],
    )
    .unwrap();
    refused(&fx, &stage2, "an older live tree under a dev-link");

    let fx = Fixture::new("heal-not-dev");
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    std::fs::remove_file(fx.seam("trust")).unwrap();
    record(&fx.layout, "trust-dev").unwrap();
    link(&stage2, &fx.seam("trust-dev"));
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(
        lines
            .iter()
            .any(|e| e.foreign_entry() == Some(("trust-dev", fx.seam("trust-dev").as_path()))),
        "{lines:?}"
    );
    assert_eq!(std::fs::read_link(fx.seam("trust-dev")).unwrap(), stage2);
    assert_eq!(replaced_from(&fx.layout, "trust-dev"), None);
}

/// An older numbered build of this store is the pre-view layout.
#[cfg(unix)]
#[test]
fn a_link_to_a_store_build_is_never_recorded_as_replaced() {
    let fx = Fixture::new("heal-store");
    let old = fx.install_trust(6808);
    dated_compiler(&fx, &old, "trustc", "2026-08-20");
    let new = fx.install_trust(6809);
    dated_compiler(&fx, &new, "trustc", "2026-09-17");
    link(&old, &fx.seam("trust"));
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(
        lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Repointed { .. })),
        "{lines:?}"
    );
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    assert_eq!(replaced_note(&fx.layout, &fx.rustup, "trust"), None);
}

/// The heal's in-use deferral.
#[cfg(unix)]
#[test]
fn the_unattended_heal_waits_out_a_build_running_through_the_tree() {
    let fx = Fixture::new("heal-in-use");
    let real = fx
        .root
        .path()
        .join("trust/build/aarch64-apple-darwin/stage2");
    dated_compiler(&fx, &real, "rustc", "2026-08-20");
    link(
        Path::new("aarch64-apple-darwin"),
        &fx.root.path().join("trust/build/host"),
    );
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    let exe = std::fs::canonicalize(stage2.join("bin").join("rustc")).unwrap();
    assert!(exe.starts_with(std::fs::canonicalize(&real).unwrap()));
    for (running, deferred) in [
        (Some(vec![exe.clone()]), Deferred::InUse(exe.clone())),
        (None, Deferred::Unknown),
    ] {
        let lines = unattended(&fx, running.clone());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            matches!(
                attached(&lines[0]),
                Some(Attached::LeftEntry { from, why, .. }) if *from == stage2 && *why == deferred
            ),
            "{lines:?}"
        );
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
        assert!(fx.seams_recorded().is_empty(), "nothing recorded");
    }
    let lines = unattended(&fx, Some(vec![PathBuf::from("/usr/bin/true")]));
    assert!(
        lines.len() == 1 && replaced_older(&lines[0]).is_some(),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust")
    );
}

/// The undo.
#[cfg(unix)]
#[test]
fn a_link_put_back_after_the_heal_is_left_by_every_pass_until_repair() {
    let fx = Fixture::new("heal-undo");
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    let healed = unattended(&fx, Some(Vec::new()));
    assert!(replaced_older(&healed[0]).is_some(), "{healed:?}");

    let out = std::process::Command::new("ln")
        .arg("-sfn")
        .arg(&stage2)
        .arg(fx.seam("trust"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);

    let mut said = String::new();
    for pass in 0..2 {
        let lines = unattended(&fx, Some(Vec::new()));
        assert!(
            lines.len() == 1
                && matches!(
                    refusal(&lines[0]),
                    Some(Refusal::PutBack { target, .. }) if *target == stage2
                ),
            "pass {pass}: {lines:?}"
        );
        said = lines[0].to_string();
        assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
    }
    let refused = refusals(&fx.layout);
    assert!(
        refused.len() == 1 && refused[0].1 == said,
        "the refusal recorded is the one said: {refused:?}"
    );
    let note = replaced_note(&fx.layout, &fx.rustup, "trust").expect("a note");
    assert!(
        note.contains(&format!("names {} again", stage2.display()))
            && note.contains("no unattended pass re-points it"),
        "{note}"
    );

    let lines = repair(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1 && replaced_older(&lines[0]).is_some(),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust")
    );
    assert!(refusals(&fx.layout).is_empty());
    assert_eq!(replaced_from(&fx.layout, "trust"), Some(stage2.clone()));

    let other = fx.root.path().join("trust2/build/host/stage2");
    dated_compiler(&fx, &other, "rustc", "2026-08-21");
    std::fs::remove_file(fx.seam("trust")).unwrap();
    link(&other, &fx.seam("trust"));
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(
        lines.len() == 1 && replaced_older(&lines[0]) == Some(other.as_path()),
        "{lines:?}"
    );
    assert_eq!(replaced_from(&fx.layout, "trust"), Some(other));
}

/// The hold is on the tree.
#[cfg(unix)]
#[test]
fn a_link_put_back_with_rustups_own_spelling_is_held() {
    let fx = Fixture::new("heal-undo-spelling");
    let build = fx.install_trust(9192);
    dated_compiler(&fx, &build, "trustc", "2026-09-17");
    let root = std::fs::canonicalize(fx.root.path()).unwrap();
    let real = root.join("trust/build/aarch64-apple-darwin/stage2");
    dated_compiler(&fx, &real, "rustc", "2026-08-20");
    link(
        Path::new("aarch64-apple-darwin"),
        &root.join("trust/build/host"),
    );
    link(&root.join("trust/build/host/stage2"), &fx.seam("trust"));
    let healed = unattended(&fx, Some(Vec::new()));
    assert!(replaced_older(&healed[0]).is_some(), "{healed:?}");

    let rustups = real.join(".");
    std::fs::remove_file(fx.seam("trust")).unwrap();
    link(&rustups, &fx.seam("trust"));
    let note = replaced_note(&fx.layout, &fx.rustup, "trust");
    assert!(
        note.as_deref()
            .is_some_and(|n| n.contains("no unattended pass re-points it")),
        "{note:?}"
    );
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(lines.len() == 1 && is_put_back(&lines[0]), "{lines:?}");
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), rustups);
}

/// A heal whose swap fails drops its in-flight record and leaves an earlier heal's way back as
/// it was.
#[cfg(unix)]
#[test]
fn a_heal_whose_swap_fails_leaves_the_record_as_it_stood() {
    let fx = Fixture::new("heal-swap-fails");
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    let earlier = fx.root.path().join("earlier/build/host/stage2");
    set_replacing(&fx.layout, "trust", Some(&earlier)).unwrap();
    record(&fx.layout, "trust").unwrap();
    assert_eq!(replaced_from(&fx.layout, "trust"), Some(earlier.clone()));
    let toolchains = toolchains_dir(&fx.rustup);
    std::fs::set_permissions(&toolchains, std::fs::Permissions::from_mode(0o555)).unwrap();
    let lines = unattended(&fx, Some(Vec::new()));
    std::fs::set_permissions(&toolchains, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::Io(_))),
        "the swap was refused by the filesystem: {lines:?}"
    );
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
    assert_eq!(replaced_from(&fx.layout, "trust"), Some(earlier));
    assert!(
        !fx.seams_recorded()
            .iter()
            .any(|k| k.starts_with(REPLACING_PREFIX)),
        "{:?}",
        fx.seams_recorded()
    );
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(
        lines.len() == 1 && replaced_older(&lines[0]).is_some(),
        "{lines:?}"
    );
    assert_eq!(replaced_from(&fx.layout, "trust"), Some(stage2));
}

/// A heal cut off before its rename (only the in-flight record written) is finished by the next
/// pass.
#[cfg(unix)]
#[test]
fn a_heal_cut_off_before_or_after_its_rename_is_finished_by_the_next_pass() {
    let fx = Fixture::new("heal-cut-off");
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    set_replacing(&fx.layout, "trust", Some(&stage2)).unwrap();
    assert_eq!(replaced_from(&fx.layout, "trust"), None);
    assert_eq!(
        replaced_note(&fx.layout, &fx.rustup, "trust"),
        None,
        "nobody put anything back"
    );
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(
        lines.len() == 1 && replaced_older(&lines[0]).is_some(),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust")
    );
    assert_eq!(
        fx.seams_recorded(),
        vec![
            format!("replaced:rustup:trust: {}", stage2.display()),
            "rustup:trust".to_string()
        ]
    );

    let fx = Fixture::new("heal-cut-off-late");
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    refresh_view(&fx.layout, "trust").unwrap();
    set_replacing(&fx.layout, "trust", Some(&stage2)).unwrap();
    crate::activate::atomic_symlink(&seam_target(&fx.layout, "trust"), &fx.seam("trust")).unwrap();
    let _ = unattended(&fx, Some(Vec::new()));
    assert_eq!(replaced_from(&fx.layout, "trust"), Some(stage2.clone()));
    assert!(
        replaced_note(&fx.layout, &fx.rustup, "trust")
            .is_some_and(|n| n.contains("was re-pointed at the store's view")),
    );
    crate::activate::atomic_symlink(&stage2, &fx.seam("trust")).unwrap();
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(lines.len() == 1 && is_put_back(&lines[0]), "{lines:?}");
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
}

/// An in-flight record left by a heal that never renamed is never settled later as a
/// replacement.
#[cfg(unix)]
#[test]
fn a_heal_cut_off_before_its_rename_and_never_finished_replaced_nothing() {
    let fx = Fixture::new("heal-leftover");
    let stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    set_replacing(&fx.layout, "trust", Some(&stage2)).unwrap();
    dated_compiler(&fx, &stage2, "rustc", "2026-09-20");
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(is_foreign(&lines[0]), "{lines:?}");
    std::fs::remove_file(fx.seam("trust")).unwrap();
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(
        matches!(attached(&lines[0]), Some(Attached::Created { .. })),
        "{lines:?}"
    );
    assert_eq!(replaced_note(&fx.layout, &fx.rustup, "trust"), None);

    dated_compiler(&fx, &stage2, "rustc", "2026-08-20");
    std::fs::remove_file(fx.seam("trust")).unwrap();
    link(&stage2, &fx.seam("trust"));
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(
        lines.len() == 1 && replaced_older(&lines[0]).is_some(),
        "healed, not held: {lines:?}"
    );
}

/// The entry is read again before the swap.
#[cfg(unix)]
#[test]
fn a_link_changed_while_the_view_is_laid_is_left_for_the_next_pass() {
    let fx = Fixture::new("heal-raced");
    let _stage2 = stale_tree_fixture(&fx, "trust/build/host/stage2", "2026-08-20");
    let seal = fx.root.path().join("toolchains").join("trust-0926abcdef");
    dated_compiler(&fx, &seal, "rustc", "2026-09-26");
    let entry = fx.seam("trust");
    let meanwhile = |layout: &Layout, name: &str| {
        let laid = refresh_view(layout, name);
        crate::activate::atomic_symlink(&seal, &entry).unwrap();
        laid
    };
    let lines = reassert_with(
        &fx.layout,
        &fx.rustup,
        &meanwhile,
        &|| Some(Vec::new()),
        false,
    );
    assert!(
        lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::Io(_))),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_link(&entry).unwrap(),
        seal,
        "their link stands"
    );
    assert_eq!(replaced_from(&fx.layout, "trust"), None);
    assert!(
        !fx.seams_recorded()
            .iter()
            .any(|k| k.starts_with(REPLACING_PREFIX) || k.starts_with(REFUSED_PREFIX)),
        "a transient, not a posture: {:?}",
        fx.seams_recorded()
    );
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(is_foreign(&lines[0]), "{lines:?}");
    assert_eq!(std::fs::read_link(&entry).unwrap(), seal);
}

/// The printed way back runs as printed.
#[cfg(unix)]
#[test]
fn the_printed_way_back_puts_back_a_path_with_a_quote_in_it() {
    let fx = Fixture::new("heal-quote");
    let stage2 = stale_tree_fixture(&fx, "o'neil's trust/build/host/stage2", "2026-08-20");
    let lines = unattended(&fx, Some(Vec::new()));
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = lines[0].to_string();
    let undo = line
        .split("to put it back: ")
        .nth(1)
        .and_then(|rest| rest.split(" (no unattended pass").next())
        .expect("the way back is on the line");
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(undo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{undo}: {out:?}");
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), stage2);
    let lines = unattended(&fx, Some(Vec::new()));
    assert!(is_put_back(&lines[0]), "{lines:?}");
}

/// The same guard on the other re-point.
#[cfg(unix)]
#[test]
fn a_pre_view_store_link_a_live_build_runs_through_is_left_until_it_exits() {
    let fx = Fixture::new("repoint-in-use");
    let old = fx.install_trust(6808);
    link(&old, &fx.seam("trust"));
    fx.install_trust(6809);
    let exe = std::fs::canonicalize(old.join("bin").join("trustc")).unwrap();
    let lines = reassert_with(
        &fx.layout,
        &fx.rustup,
        &refresh_view,
        &|| Some(vec![exe.clone()]),
        false,
    );
    assert!(
        lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::LeftEntry { .. })),
        "{lines:?}"
    );
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), old);
    let lines = reassert_with(
        &fx.layout,
        &fx.rustup,
        &refresh_view,
        &|| Some(Vec::new()),
        false,
    );
    assert!(
        lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Repointed { .. })),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust")
    );
}

/// A pre-view link to the view's own build (`store/trust/current`, or its number) is re-pointed
/// even while a process runs from that build or the table cannot be read.
#[cfg(unix)]
#[test]
fn a_pre_view_link_to_the_views_own_build_is_repointed_under_a_running_build() {
    let fx = Fixture::new("repoint-same-build");
    let build = fx.install_trust(6809);
    let exe = std::fs::canonicalize(build.join("bin").join("targo")).unwrap();
    for spelling in [store_current(&fx.layout), build.clone()] {
        for running in [Some(vec![exe.clone()]), None] {
            let _ = std::fs::remove_file(fx.seam("trust"));
            link(&spelling, &fx.seam("trust"));
            let lines = reassert_with(
                &fx.layout,
                &fx.rustup,
                &refresh_view,
                &|| running.clone(),
                false,
            );
            assert!(
                lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Repointed { .. })),
                "{spelling:?} with {running:?}: {lines:?}"
            );
            assert_eq!(
                std::fs::read_link(fx.seam("trust")).unwrap(),
                seam_target(&fx.layout, "trust")
            );
            assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
        }
    }
}

#[cfg(unix)]
#[test]
fn detach_removes_only_owned_links() {
    let fx = Fixture::new("detach");
    fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let out = detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
    assert!(matches!(out, Detached::Removed { .. }), "{out}");
    assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
    assert!(fx.seams_recorded().is_empty());
    assert!(std::fs::symlink_metadata(store_current(&fx.layout)).is_ok());
    assert!(std::fs::symlink_metadata(view_dir(&fx.layout, "trust")).is_err());

    let out = detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
    assert!(matches!(out, Detached::AlreadyAbsent { .. }), "{out}");

    let elsewhere = fx.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    link(&elsewhere, &fx.seam("trust"));
    let err = detach(&fx.layout, &fx.rustup, "trust", false).unwrap_err();
    assert!(matches!(err, Refusal::Foreign { .. }), "{err}");
    assert_eq!(std::fs::read_link(fx.seam("trust")).unwrap(), elsewhere);
    let out = detach(&fx.layout, &fx.rustup, "trust", true).unwrap();
    assert!(matches!(out, Detached::ForcedUnlinked { .. }), "{out}");
    assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
    assert!(
        elsewhere.is_dir(),
        "the target of a foreign link is never touched"
    );

    std::fs::create_dir_all(fx.seam("trust").join("bin")).unwrap();
    std::fs::write(fx.seam("trust").join("bin").join("rustc"), b"x").unwrap();
    let err = detach(&fx.layout, &fx.rustup, "trust", false).unwrap_err();
    assert!(matches!(err, Refusal::Foreign { .. }), "{err}");
    let out = detach(&fx.layout, &fx.rustup, "trust", true).unwrap();
    let moved_to = match &out {
        Detached::ForcedDisplaced { moved_to, .. } => moved_to.clone(),
        other => panic!("expected ForcedDisplaced, got {other}"),
    };
    assert!(
        std::fs::symlink_metadata(fx.seam("trust")).is_err(),
        "seam path is clear"
    );
    assert!(
        moved_to.join("bin").join("rustc").is_file(),
        "the foreign tree survives beside it: {}",
        moved_to.display()
    );
    assert!(moved_to.starts_with(toolchains_dir(&fx.rustup)));
    let out = attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(matches!(out, Attached::Created { .. }), "{out}");
}

#[cfg(unix)]
#[test]
fn detach_with_no_rustup_clears_the_record() {
    let fx = Fixture::new("detach-norustup");
    fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    std::fs::remove_dir_all(fx.rustup.join("toolchains")).unwrap();
    let out = detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
    assert!(matches!(out, Detached::NoRustup { .. }), "{out}");
    assert!(fx.seams_recorded().is_empty());
    assert!(!fx.rustup.join("toolchains").exists(), "never created");
}

#[cfg(unix)]
#[test]
fn record_never_clobbers_other_status_fields() {
    let fx = Fixture::new("record");
    fx.install_trust(6808);
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "trust".to_string(),
        crate::ProgramStatus {
            installed_build: Some(6808),
            state: "managed 6808 — pinned by index 9".into(),
            tree_root: "abc".into(),
        },
    );
    crate::status::write(
        &fx.layout,
        &crate::Status {
            schema: 1,
            updated_at: "2026-08-29T00:00:00Z".into(),
            enabled: true,
            index_source: "owner/repo".into(),
            outcome: "up to date".into(),
            programs,
            ..Default::default()
        },
    )
    .unwrap();
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let back = crate::status::read(&fx.layout).unwrap();
    assert_eq!(back.seams, vec!["rustup:trust".to_string()]);
    assert_eq!(back.updated_at, "2026-08-29T00:00:00Z");
    assert!(back.enabled);
    assert_eq!(back.index_source, "owner/repo");
    assert_eq!(back.outcome, "up to date");
    assert_eq!(back.programs["trust"].installed_build, Some(6808));
    assert_eq!(back.programs["trust"].tree_root, "abc");
    detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
    let back = crate::status::read(&fx.layout).unwrap();
    assert!(back.seams.is_empty());
    assert_eq!(back.programs["trust"].installed_build, Some(6808));
}

#[test]
fn recording_a_seam_heals_an_unreadable_record() {
    let prefix = crate::testdir::dir("atpkg-seam-unreadable-");
    let layout = Layout {
        prefix: prefix.to_path_buf(),
    };
    let corrupt = "seams = [\"rustup:trust\"]\nthis is not valid toml {{{\n";
    std::fs::write(layout.status(), corrupt).unwrap();
    record(&layout, "trust").expect("the recorder heals and records");
    assert_eq!(recorded_keys(&layout), vec![record_key("trust")]);
    let aside = std::fs::read_dir(&layout.prefix)
        .unwrap()
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("status.toml.corrupt-")
        })
        .expect("the unreadable record is kept aside");
    assert_eq!(std::fs::read_to_string(aside.path()).unwrap(), corrupt);
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

#[cfg(unix)]
#[test]
fn trust_dev_is_a_second_independent_seam() {
    let fx = Fixture::new("trustdev");
    fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    attach(&fx.layout, &fx.rustup, "trust-dev").unwrap();
    assert_eq!(
        fx.seams_recorded(),
        vec!["rustup:trust".to_string(), "rustup:trust-dev".to_string()]
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust-dev")).unwrap(),
        seam_target(&fx.layout, "trust-dev")
    );
    detach(&fx.layout, &fx.rustup, "trust-dev", false).unwrap();
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    assert!(
        std::fs::symlink_metadata(fx.seam("trust")).is_ok(),
        "the other seam stays"
    );
}

#[cfg(unix)]
#[test]
fn reassert_recreates_recorded_and_first_attaches_trust() {
    let fx = Fixture::new("reassert");
    fx.install_trust(6808);
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(attached(&lines[0]), Some(Attached::Created { .. })),
        "{lines:?}"
    );
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    std::fs::remove_file(fx.seam("trust")).unwrap();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(std::fs::symlink_metadata(fx.seam("trust")).is_ok());
    std::fs::remove_file(fx.seam("trust")).unwrap();
    std::fs::create_dir_all(fx.seam("trust")).unwrap();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(is_foreign(&lines[0]), "{lines:?}");
    assert!(fx.seam("trust").is_dir(), "left alone");
}

#[cfg(unix)]
#[test]
fn reassert_first_attach_needs_trust_installed() {
    let fx = Fixture::new("reassert-notinstalled");
    assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
    assert!(fx.seams_recorded().is_empty());
}

#[cfg(unix)]
#[test]
fn detach_recorded_walks_only_the_record() {
    let fx = Fixture::new("detach-recorded");
    fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let elsewhere = fx.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    link(&elsewhere, &fx.seam("trust-dev"));
    let lines = detach_recorded(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(lines[0], SeamEvent::Detached(Detached::Removed { .. })),
        "{lines:?}"
    );
    assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
    assert_eq!(std::fs::read_link(fx.seam("trust-dev")).unwrap(), elsewhere);
    assert!(fx.seams_recorded().is_empty());
}

#[test]
fn unknown_record_keys_are_ignored_never_acted_on() {
    let fx = Fixture::new("unknown-keys");
    crate::status::write(
        &fx.layout,
        &crate::Status {
            schema: 1,
            seams: vec![
                "rustup:stable".into(),
                "path:/usr/local/bin".into(),
                "rustup:trust".into(),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(recorded_names(&fx.layout), vec!["trust".to_string()]);
}

/// The view.
#[cfg(unix)]
#[test]
fn the_view_is_a_clone_of_the_build_with_the_stock_names_laid() {
    let fx = Fixture::new("view");
    let build = fx.install_trust(6808);
    let r = refresh_view(&fx.layout, "trust").unwrap();
    assert_eq!(r.build, build);
    assert_eq!(r.tools, 4);
    assert_eq!(
        r.stock,
        vec![
            ("rustc", "trustc"),
            ("cargo", "targo"),
            ("rustdoc", "trustdoc")
        ]
    );
    assert!(r.changed);
    let bin = view_dir(&fx.layout, "trust").join("bin");
    for (public, trust) in STOCK_NAMES {
        assert!(
            cloned(&build.join("bin").join(trust), &bin.join(public)),
            "{public} is a clone of the store's {trust}"
        );
    }
    assert!(cloned(&build.join("bin").join("tippy"), &bin.join("tippy")));
    let lib = view_dir(&fx.layout, "trust").join("lib");
    assert!(
        std::fs::symlink_metadata(&lib).unwrap().is_dir(),
        "a real directory"
    );
    assert!(cloned(
        &build.join("lib").join("libtrust.dylib"),
        &lib.join("libtrust.dylib")
    ));
    for tool in ["trustc", "targo", "trustdoc", "tippy"] {
        use std::os::unix::fs::MetadataExt as _;
        let nlink = std::fs::symlink_metadata(build.join("bin").join(tool))
            .unwrap()
            .nlink();
        assert_eq!(nlink, 1, "the store's {tool} has {nlink} links");
    }
    assert_eq!(
        std::fs::read_link(lib.join("rustlib").join("etc-link")).unwrap(),
        Path::new("../../etc")
    );
    assert!(!refresh_view(&fx.layout, "trust").unwrap().changed);
}

/// A build laid aside in `<build>.noindex` ([`aterm_toolchain::layout`]) is read through
/// its `<build>` link: moving it changes nothing the view holds, and a build that arrives
/// aside is presented like any other.
#[cfg(unix)]
#[test]
fn the_view_reads_a_build_laid_aside_through_its_link() {
    let fx = Fixture::new("view-aside");
    let build = fx.install_trust(6808);
    assert!(refresh_view(&fx.layout, "trust").unwrap().changed);
    crate::store::lay_aside(&build).unwrap();
    let again = refresh_view(&fx.layout, "trust").unwrap();
    assert_eq!(
        again.build, build,
        "the view names the build, not its folder"
    );
    assert!(!again.changed, "the same tree, so the same view");

    let next = fx.install_trust(6809);
    crate::store::lay_aside(&next).unwrap();
    let r = refresh_view(&fx.layout, "trust").unwrap();
    assert_eq!(r.build, next);
    assert!(r.changed);
    let view = view_dir(&fx.layout, "trust");
    assert!(
        std::fs::symlink_metadata(view.join("lib"))
            .unwrap()
            .is_dir()
    );
    assert!(cloned(
        &next.join("lib").join("libtrust.dylib"),
        &view.join("lib").join("libtrust.dylib")
    ));
    assert!(cloned(
        &next.join("bin").join("trustc"),
        &view.join("bin").join("rustc")
    ));
}

/// The view as an atpkg before the Spotlight layout laid it: a real directory at its
/// name, from the view this one laid aside.
#[cfg(target_os = "macos")]
fn make_plain(view: &Path) {
    let aside = view.with_file_name(format!(
        "{}.noindex",
        view.file_name().unwrap().to_str().unwrap()
    ));
    std::fs::remove_file(view).unwrap();
    std::fs::rename(&aside, view).unwrap();
    assert!(is_real_dir(view), "PRECONDITION: a plain view");
}

/// The rustup views Spotlight still indexes, by name.
#[cfg(target_os = "macos")]
fn exposed_views(fx: &Fixture) -> Vec<String> {
    crate::store::spotlight_exposed_views(&fx.layout.prefix)
        .into_iter()
        .map(|v| v.name)
        .collect()
}

/// A view laid where none stood is laid whole in `<name>.noindex` and only then named by
/// its `<name>` link, and every reader goes through the link: the seam, rustup's
/// `toolchains/<name>`, a compiler's canonical sysroot, its lease.
#[cfg(target_os = "macos")]
#[test]
fn a_new_view_is_laid_aside_behind_its_link() {
    let fx = Fixture::new("view-new-aside");
    let build = fx.install_trust(15698);
    let view = view_dir(&fx.layout, "trust");
    let aside = views_root(&fx.layout).join("trust.noindex");

    let attached = attach(&fx.layout, &fx.rustup, "trust").unwrap();
    assert!(matches!(attached, Attached::Created { .. }), "{attached:?}");
    assert_eq!(
        std::fs::read_link(&view).unwrap(),
        Path::new("trust.noindex")
    );
    assert!(is_real_dir(&aside) && is_real_dir(&aside.join("lib/rustlib")));
    assert!(exposed_views(&fx).is_empty(), "Spotlight skips it");
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        view,
        "rustup's toolchain names the view, not its folder"
    );
    let rustc = fx.seam("trust").join("bin").join("rustc");
    assert!(cloned(&build.join("bin").join("trustc"), &rustc));
    let sysroot = std::fs::canonicalize(fx.seam("trust")).unwrap();
    assert_eq!(sysroot, std::fs::canonicalize(&aside).unwrap());
    assert_eq!(
        crate::lease::Subject::of_dir(&fx.layout.prefix, &sysroot.join("bin")),
        crate::lease::Subject::view("trust"),
        "the sysroot a compiler reports names the view"
    );
    let mode = std::fs::symlink_metadata(&aside)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o700);
    let again = refresh_view_with(&fx.layout, "trust", &|| None).unwrap();
    assert!(
        !again.changed && again.deferred.is_none(),
        "a view current behind its link is current, with no table to read: {again:?}"
    );

    let r = refresh_view_with(&fx.layout, "trust", &|| {
        Some(vec![sysroot.join("bin").join("rustc")])
    })
    .unwrap();
    assert!(!r.changed, "nothing to lay: {r:?}");
    fx.install_trust(15700);
    let r = refresh_view_with(&fx.layout, "trust", &|| {
        Some(vec![sysroot.join("bin").join("rustc")])
    })
    .unwrap();
    assert!(
        matches!(r.deferred, Some(Deferred::InUse(_))),
        "a compiler running from the folder holds the view: {r:?}"
    );
    let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
    assert!(r.changed && r.deferred.is_none(), "{r:?}");
    assert!(aterm_toolchain::layout::is_linked_aside(&view) && is_real_dir(&aside));
    assert_eq!(
        std::fs::read_to_string(rustc).unwrap(),
        "trustc of build 15700"
    );
}

/// A view an older atpkg laid plain stays where it is while it matches its build or
/// something runs from it, and is laid anew aside where atpkg lays it anyway.
#[cfg(target_os = "macos")]
#[test]
fn a_plain_view_moves_aside_only_when_it_is_laid_again() {
    #[cfg(unix)]
    if !crate::testdir::isolated_lifecycle(
        "seam::tests::a_plain_view_moves_aside_only_when_it_is_laid_again",
    ) {
        return;
    }
    let fx = Fixture::new("view-plain");
    fx.install_trust(9192);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    let aside = views_root(&fx.layout).join("trust.noindex");
    make_plain(&view);
    let stamps = inode_stamps(&view);
    assert_eq!(exposed_views(&fx), ["trust"]);

    let r = refresh_view(&fx.layout, "trust").unwrap();
    assert!(
        !r.changed && is_real_dir(&view),
        "a view that matches is left: {r:?}"
    );
    assert_eq!(inode_stamps(&view), stamps, "and not one file of it moves");

    let new = fx.install_trust(15698);
    let running = view.join("bin").join("rustc");
    let r = refresh_view_with(&fx.layout, "trust", &|| Some(vec![running.clone()])).unwrap();
    assert!(matches!(r.deferred, Some(Deferred::InUse(_))), "{r:?}");
    assert!(
        is_real_dir(&view) && !aside.exists(),
        "a view in use is never moved"
    );
    let r = refresh_view_with(&fx.layout, "trust", &|| None).unwrap();
    assert_eq!(r.deferred, Some(Deferred::Unknown));
    assert!(is_real_dir(&view) && !aside.exists());
    let lease = crate::lease::take(
        &fx.layout.prefix,
        &crate::lease::Subject::view("trust").unwrap(),
        "targo (pid 7) in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
    assert!(matches!(r.deferred, Some(Deferred::Leased(_))), "{r:?}");
    assert!(is_real_dir(&view) && !aside.exists());
    lease.release();

    let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
    assert!(r.changed && r.deferred.is_none(), "{r:?}");
    assert!(aterm_toolchain::layout::is_linked_aside(&view) && is_real_dir(&aside));
    assert!(exposed_views(&fx).is_empty());
    assert!(cloned(
        &new.join("bin").join("trustc"),
        &view.join("bin").join("rustc")
    ));
    assert!(view_matches(&new, &view, Depth::Deep));
    let debris: Vec<_> = std::fs::read_dir(views_root(&fx.layout))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        debris.len(),
        2,
        "the view and its link, nothing parked: {debris:?}"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        view,
        "rustup's link is never rewritten"
    );
}

/// The crash windows of laying a view aside, each put in order by the next refresh: an
/// aside with no link gets it, an aside a cut-short lay left beside a plain view goes, and
/// so do a view half laid and a plain view parked for the link.
#[cfg(target_os = "macos")]
#[test]
fn a_view_lay_killed_midway_is_put_in_order() {
    let fx = Fixture::new("view-crash");
    let build = fx.install_trust(15698);
    refresh_view(&fx.layout, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    let aside = views_root(&fx.layout).join("trust.noindex");

    std::fs::remove_file(&view).unwrap();
    let r = refresh_view(&fx.layout, "trust").unwrap();
    assert!(!r.changed, "the whole view gets its link back: {r:?}");
    assert!(aterm_toolchain::layout::is_linked_aside(&view));

    make_plain(&view);
    std::fs::create_dir_all(aside.join("lib")).unwrap();
    let parked = views_root(&fx.layout).join(".trust.old-4242");
    std::fs::create_dir_all(parked.join("bin")).unwrap();
    let laying = views_root(&fx.layout).join(".trust.lay-4242");
    std::fs::create_dir_all(laying.join("lib")).unwrap();
    let r = refresh_view(&fx.layout, "trust").unwrap();
    assert!(!r.changed, "{r:?}");
    assert!(is_real_dir(&view) && view_matches(&build, &view, Depth::Deep));
    assert!(!aside.exists(), "a lay cut short goes");
    assert!(!parked.exists(), "a parked view goes");
    assert!(!laying.exists(), "a view half laid goes");

    let kept = views_root(&fx.layout).join(".trust-dev.old-4242");
    std::fs::create_dir_all(&kept).unwrap();
    refresh_view(&fx.layout, "trust").unwrap();
    assert!(
        kept.exists(),
        "another view's parking is that view's to settle"
    );
}

/// Detaching a view laid aside removes it whole: its link and its folder.
#[cfg(target_os = "macos")]
#[test]
fn detaching_a_view_laid_aside_removes_its_link_and_folder() {
    let fx = Fixture::new("view-detach-aside");
    fx.install_trust(15698);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let aside = views_root(&fx.layout).join("trust.noindex");
    assert!(is_real_dir(&aside));
    let out = detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
    assert!(matches!(out, Detached::Removed { .. }), "{out}");
    assert!(std::fs::symlink_metadata(view_dir(&fx.layout, "trust")).is_err());
    assert!(
        std::fs::symlink_metadata(&aside).is_err(),
        "the folder goes too"
    );
}

/// A stock-name copy an older bundle shipped is not presented.
#[cfg(unix)]
#[test]
fn a_stock_copy_the_bundle_shipped_is_replaced_by_a_clone_of_the_trust_tool() {
    let fx = Fixture::new("view-copy");
    let build = fx.install_trust(8595);
    std::fs::write(build.join("bin").join("rustc"), b"a second copy of trustc").unwrap();
    let r = refresh_view(&fx.layout, "trust").unwrap();
    assert_eq!(r.tools, 5);
    let bin = view_dir(&fx.layout, "trust").join("bin");
    assert!(cloned(
        &build.join("bin").join("trustc"),
        &bin.join("rustc")
    ));
    assert_eq!(
        std::fs::read(bin.join("rustc")).unwrap(),
        b"trustc of build 8595"
    );
}

/// The view follows `current`.
#[cfg(unix)]
#[test]
fn the_view_follows_current_across_an_update() {
    let fx = Fixture::new("view-update");
    let old = fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let bin = view_dir(&fx.layout, "trust").join("bin");
    assert!(cloned(&old.join("bin").join("trustc"), &bin.join("rustc")));
    let new = fx.install_trust(6809);
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(
            attached(&lines[0]),
            Some(Attached::Adopted { view_build: Some(b), .. }) if *b == new
        ),
        "{lines:?}"
    );
    assert!(
        reassert(&fx.layout, &fx.rustup).is_empty(),
        "the same build again is a quiet adoption"
    );
    assert!(cloned(&new.join("bin").join("trustc"), &bin.join("rustc")));
    assert_eq!(
        std::fs::read(bin.join("cargo")).unwrap(),
        b"targo of build 6809"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust"),
        "the rustup entry never moved; only the view's contents did"
    );
}

/// A view a live process runs from is left as it stands.
#[cfg(unix)]
#[test]
fn a_view_a_live_process_runs_from_is_left_as_it_stands() {
    let fx = Fixture::new("view-in-use");
    fx.install_trust(9178);
    let first = refresh_view_with(&fx.layout, "trust", &|| -> Option<Vec<PathBuf>> {
        panic!("a first lay has nothing a process could run from, and reads no table")
    })
    .unwrap();
    assert!(first.changed && first.deferred.is_none(), "{first:?}");
    let view = view_dir(&fx.layout, "trust");
    let reads = &std::cell::Cell::new(0_u32);
    let table = |exes: Option<Vec<PathBuf>>| {
        move || {
            reads.set(reads.get() + 1);
            exes.clone()
        }
    };
    let current = refresh_view_with(&fx.layout, "trust", &table(Some(Vec::new()))).unwrap();
    assert!(!current.changed && current.deferred.is_none());
    assert_eq!(reads.get(), 0, "a current view reads no table");

    let new = fx.install_trust(9192);
    let exe = view.join("bin").join("targo");
    let resolved = std::fs::canonicalize(&exe).unwrap();
    for (running, why) in [
        (Some(vec![exe.clone()]), Deferred::InUse(exe.clone())),
        (
            Some(vec![resolved.clone()]),
            Deferred::InUse(resolved.clone()),
        ),
        (None, Deferred::Unknown),
    ] {
        reads.set(0);
        let r = refresh_view_with(&fx.layout, "trust", &table(running)).unwrap();
        assert_eq!(reads.get(), 1, "the table is read once per refresh");
        assert!(!r.changed, "{r:?}");
        assert_eq!(r.deferred, Some(why), "{r:?}");
        assert_eq!(r.build, new, "the build it was NOT re-laid onto");
        assert_eq!(
            std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
            "trustc of build 9178",
            "the running build's sibling compiler stays"
        );
        assert_eq!(
            std::fs::read_to_string(view.join("lib").join("libtrust.dylib")).unwrap(),
            "driver of 9178",
            "and so does the driver it loads"
        );
    }

    let elsewhere = vec![
        view_dir(&fx.layout, "trust-dev").join("bin").join("targo"),
        new.join("bin").join("targo"),
    ];
    let r = refresh_view_with(&fx.layout, "trust", &table(Some(elsewhere))).unwrap();
    assert!(r.changed && r.deferred.is_none(), "{r:?}");
    assert_eq!(
        std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
        "trustc of build 9192"
    );
    assert!(view_matches(&new, &view, Depth::Deep));
}

/// A view a command under `aterm pkg lease` holds is left as it stands.
#[cfg(unix)]
#[test]
fn a_leased_view_is_left_as_it_stands_until_the_run_ends() {
    #[cfg(unix)]
    if !crate::testdir::isolated_lifecycle(
        "seam::tests::a_leased_view_is_left_as_it_stands_until_the_run_ends",
    ) {
        return;
    }
    let fx = Fixture::new("view-leased");
    fx.install_trust(9178);
    refresh_view(&fx.layout, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    let lease = crate::lease::take(
        &fx.layout.prefix,
        &crate::lease::Subject::view("trust").unwrap(),
        "targo (pid 7) — held by `aterm pkg lease` in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let new = fx.install_trust(9192);
    let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
    let Some(Deferred::Leased(holders)) = &r.deferred else {
        panic!("a leased view is left: {r:?}");
    };
    assert!(holders.clause().contains("targo (pid 7)"), "{holders:?}");
    assert_eq!(
        std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
        "trustc of build 9178",
        "the run's compiler stays"
    );
    lease.release();
    let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
    assert!(r.changed && r.deferred.is_none(), "{r:?}");
    assert!(view_matches(&new, &view, Depth::Deep));
}

/// A view left behind under a lease is recorded.
#[cfg(unix)]
#[test]
fn a_view_a_flip_left_behind_is_recorded_and_due_once_its_run_ends() {
    #[cfg(unix)]
    if !crate::testdir::isolated_lifecycle(
        "seam::tests::a_view_a_flip_left_behind_is_recorded_and_due_once_its_run_ends",
    ) {
        return;
    }
    let fx = Fixture::new("view-left-due");
    fx.install_trust(100);
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Created { .. })),
        "{lines:?}"
    );
    let view = view_dir(&fx.layout, "trust");
    let quiet = || Some(Vec::new());
    let t0 = 1_790_000_000;
    let lease = crate::lease::take(
        &fx.layout.prefix,
        &crate::lease::Subject::view("trust").unwrap(),
        "targo (pid 7) — held by `aterm pkg lease` in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let new = fx.install_trust(200);
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines
            .iter()
            .any(|e| matches!(e, SeamEvent::ViewLeft { .. })),
        "{lines:?}"
    );
    let left = crate::quiet::views_left(&fx.layout);
    assert_eq!(
        left.views.get("trust").map(|v| v.build.clone()),
        Some(new.display().to_string()),
        "the view left behind is recorded: {left:?}"
    );
    assert_eq!(
        crate::quiet::due_with(&fx.layout, t0, &quiet),
        None,
        "nothing is due while the run holds the view"
    );
    lease.release();
    let due = crate::quiet::due_with(&fx.layout, t0, &quiet);
    assert_eq!(
        due.map(|d| d.why),
        Some(crate::quiet::Due::View),
        "due the moment the run ends"
    );
    let exe = view.join("bin").join("targo");
    assert_eq!(
        crate::quiet::due_with(&fx.layout, t0, &|| Some(vec![exe.clone()])),
        None
    );
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        !lines
            .iter()
            .any(|e| matches!(e, SeamEvent::ViewLeft { .. })),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
        "trustc of build 200"
    );
    assert!(crate::quiet::views_left(&fx.layout).views.is_empty());
    assert_eq!(crate::quiet::due_with(&fx.layout, t0, &quiet), None);
}

/// With `~/.rustup/toolchains` gone no re-assertion reaches a seam.
#[cfg(unix)]
#[test]
fn a_view_left_behind_ends_when_rustup_is_gone() {
    let fx = Fixture::new("view-left-no-rustup");
    fx.install_trust(100);
    let _ = reassert(&fx.layout, &fx.rustup);
    let lease = crate::lease::take(
        &fx.layout.prefix,
        &crate::lease::Subject::view("trust").unwrap(),
        "targo (pid 7) — held by `aterm pkg lease` in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    fx.install_trust(200);
    let _ = reassert(&fx.layout, &fx.rustup);
    assert!(
        crate::quiet::views_left(&fx.layout)
            .views
            .contains_key("trust"),
        "the view is left behind and recorded"
    );
    let _ = reassert(&fx.layout, &fx.rustup);
    assert!(
        crate::quiet::views_left(&fx.layout)
            .views
            .contains_key("trust"),
        "the negative control: with rustup there, the record stands"
    );
    std::fs::remove_dir_all(fx.rustup.join("toolchains")).unwrap();
    assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    assert!(
        crate::quiet::views_left(&fx.layout).views.is_empty(),
        "rustup gone: nothing waits for a view"
    );
    assert!(!crate::quiet::view_record_path(&fx.layout).exists());
    drop(lease);
}

/// The dev-link arm takes a clone view down to lay its stubs.
#[cfg(unix)]
#[test]
fn a_dev_link_does_not_take_down_a_view_a_build_runs_from() {
    let fx = Fixture::new("dev-link-in-use");
    let store = fx.install_trust(6808);
    refresh_view(&fx.layout, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    let exe = view.join("bin").join("targo");
    let checkout = sysroot_checkout(&fx, "stage2");
    crate::linkmode::link(
        &fx.layout,
        "trust",
        &checkout,
        &[PathBuf::from("bin/trustc"), PathBuf::from("bin/targo")],
    )
    .unwrap();
    let r = refresh_view_with(&fx.layout, "trust", &|| Some(vec![exe.clone()])).unwrap();
    assert_eq!(r.deferred, Some(Deferred::InUse(exe.clone())), "{r:?}");
    assert_eq!(r.build, checkout);
    assert!(cloned(
        &store.join("bin").join("trustc"),
        &view.join("bin").join("trustc")
    ));
    assert!(
        is_real_dir(&view.join("lib")),
        "the clone view's lib/ stays"
    );
    let r = refresh_view_with(&fx.layout, "trust", &|| Some(Vec::new())).unwrap();
    assert!(r.changed && r.deferred.is_none(), "{r:?}");
    assert_eq!(
        std::fs::read_link(view.join("lib")).unwrap(),
        checkout.join("lib")
    );
    crate::linkmode::unlink(&fx.layout, "trust").unwrap();
}

/// A pass that leaves the view behind still clears a refusal the disk has answered.
#[cfg(unix)]
#[test]
fn a_deferral_keeps_no_refusal_the_disk_answered_and_a_failed_relay_records_none() {
    let fx = Fixture::new("left-view-refusal");
    fx.install_trust(6808);
    assert_eq!(
        reassert(&fx.layout, &fx.rustup).len(),
        1,
        "the first attach"
    );
    let exe = view_dir(&fx.layout, "trust").join("bin").join("targo");
    let checkout = sysroot_checkout(&fx, "stage2");
    let aside = fx.root.path().join("stage2-lib-aside");
    std::fs::rename(checkout.join("lib"), &aside).unwrap();
    crate::linkmode::link(
        &fx.layout,
        "trust",
        &checkout,
        &[PathBuf::from("bin/trustc")],
    )
    .unwrap();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::LinkedNoSysroot { .. })),
        "{lines:?}"
    );
    assert_eq!(refusals(&fx.layout).len(), 1);

    std::fs::rename(&aside, checkout.join("lib")).unwrap();
    let in_use = |l: &Layout, n: &str| refresh_view_with(l, n, &|| Some(vec![exe.clone()]));
    let lines = reassert_with(&fx.layout, &fx.rustup, &in_use, &|| Some(Vec::new()), false);
    assert!(
        lines.len() == 1
            && matches!(&lines[0], SeamEvent::ViewLeft { build, .. } if *build == checkout),
        "{lines:?}"
    );
    assert!(
        refusals(&fx.layout).is_empty(),
        "the checkout is a sysroot again, and the record says what the disk does: {:?}",
        refusals(&fx.layout)
    );

    let failed = |_: &Layout, _: &str| -> io::Result<Refreshed> {
        Err(io::Error::other("clonefile: operation not supported"))
    };
    let lines = reassert_with(&fx.layout, &fx.rustup, &failed, &|| Some(Vec::new()), false);
    assert!(
        lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::Io(_))),
        "{lines:?}"
    );
    assert!(
        refusals(&fx.layout).is_empty(),
        "{:?}",
        refusals(&fx.layout)
    );
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);
    crate::linkmode::unlink(&fx.layout, "trust").unwrap();
}

/// End to end with a real process.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn reassert_leaves_a_view_a_live_build_runs_from_and_says_so() {
    if probe_parks_in_the_view_if_asked() {
        return;
    }
    let fx = std::mem::ManuallyDrop::new(Fixture::new("view-live"));
    let old = fx.install_trust(100);
    let store_targo = old.join("bin").join("targo");
    std::fs::remove_file(&store_targo).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), &store_targo).expect("copy this test binary");
    std::fs::set_permissions(&store_targo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1 && matches!(attached(&lines[0]), Some(Attached::Created { .. })),
        "{lines:?}"
    );
    let view = view_dir(&fx.layout, "trust");
    if let Some(translator) = aterm_uds::process::translator() {
        eprintln!(
            "NOT RUN under {translator}: seam::tests::reassert_leaves_a_view_a_live_build_\
             runs_from_and_says_so: the probe's executable reads as the translator, never \
             the view's targo; a CPU of this code's architecture decides it"
        );
        drop(std::mem::ManuallyDrop::into_inner(fx));
        return;
    }

    struct Probe(aterm_uds::spawnfd::Child);
    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut probe = Probe(
        aterm_uds::spawnfd::command(view.join("bin").join("targo"))
            .args(["--exact", "--nocapture", "--quiet", VIEW_PROBE])
            .env(VIEW_PROBE_ENV, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the view's targo"),
    );
    aterm_uds::deadline::prints(
        &mut probe.0,
        VIEW_PARKED,
        aterm_uds::deadline::FIRST_EXEC_HANG,
    );
    let fx = std::mem::ManuallyDrop::into_inner(fx);
    assert_eq!(
        crate::gc::runs_from(&view),
        Some(true),
        "PRECONDITION: the process table shows the probe running from {}",
        view.display()
    );

    let new = fx.install_trust(200);
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(
            &lines[0],
            SeamEvent::ViewLeft { build, why: Deferred::InUse(_), .. } if *build == new
        ),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
        "trustc of build 100",
        "a live build's next compiler did not change under it"
    );
    assert!(cloned(&store_targo, &view.join("bin").join("targo")));

    let _ = probe.0.kill();
    let _ = probe.0.wait();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1
            && matches!(
                attached(&lines[0]),
                Some(Attached::Adopted { view_build: Some(b), .. }) if *b == new
            ),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_to_string(view.join("bin").join("trustc")).unwrap(),
        "trustc of build 200"
    );
}

/// The env that puts the copy above into its parked mode, and the case it runs there.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const VIEW_PROBE_ENV: &str = "ATPKG_SEAM_VIEW_PROBE";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const VIEW_PROBE: &str = "seam::tests::reassert_leaves_a_view_a_live_build_runs_from_and_says_so";
/// The line the probe writes once it runs from the view.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const VIEW_PARKED: &str = "parked in the view";

/// The copy's whole job.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn probe_parks_in_the_view_if_asked() -> bool {
    if std::env::var_os(VIEW_PROBE_ENV).is_none() {
        return false;
    }
    println!("{VIEW_PARKED}");
    // SAFETY: `getppid` takes no arguments, reads no memory we own, and cannot fail.
    let spawner = unsafe { libc::getppid() };
    loop {
        std::thread::sleep(std::time::Duration::from_millis(50));
        // SAFETY: `getppid` is safe to call as above.
        if unsafe { libc::getppid() } != spawner {
            return true;
        }
    }
}

/// A dev checkout shaped like a sysroot.
#[cfg(unix)]
fn sysroot_checkout(fx: &Fixture, label: &str) -> PathBuf {
    let dir = fx.root.path().join(label);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::create_dir_all(dir.join("share")).unwrap();
    std::fs::write(
        dir.join("lib").join("libtrust.dylib"),
        "driver of the checkout",
    )
    .unwrap();
    for tool in ["trustc", "targo", "tippy"] {
        std::fs::write(dir.join("bin").join(tool), format!("{tool} of {label}")).unwrap();
    }
    dir
}

/// A dev-linked trust is what rustup's `trust` presents.
#[cfg(unix)]
#[test]
fn a_dev_linked_trust_is_what_rustup_presents_until_it_is_unlinked() {
    let fx = Fixture::new("dev-link");
    let store = fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    let bin = view.join("bin");
    assert!(cloned(
        &store.join("bin").join("trustc"),
        &bin.join("rustc")
    ));
    assert_eq!(view_source(&fx.layout), ViewSource::Store);

    let checkout = sysroot_checkout(&fx, "stage2");
    crate::linkmode::link(
        &fx.layout,
        "trust",
        &checkout,
        &[PathBuf::from("bin/trustc"), PathBuf::from("bin/targo")],
    )
    .unwrap();
    assert_eq!(
        view_source(&fx.layout),
        ViewSource::Linked(checkout.clone())
    );
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(
            attached(&lines[0]),
            Some(Attached::Adopted { view_build: Some(b), .. }) if *b == checkout
        ),
        "{lines:?}"
    );
    for (name, target) in [
        ("trustc", "trustc"),
        ("targo", "targo"),
        ("tippy", "tippy"),
        ("rustc", "trustc"),
        ("cargo", "targo"),
    ] {
        let stub = bin.join(name);
        assert!(
            std::fs::symlink_metadata(&stub).unwrap().is_file(),
            "{name} is a stub, not a link: targo and tippy refuse a symlinked executable"
        );
        assert_eq!(
            crate::platform::resolve_shim(&stub).unwrap(),
            checkout.join("bin").join(target),
            "{name} execs the checkout's tool"
        );
    }
    assert!(
        std::fs::read_dir(&bin).unwrap().count() == 5,
        "nothing extra in the view's bin"
    );
    assert_eq!(
        std::fs::read_link(view.join("lib")).unwrap(),
        checkout.join("lib")
    );
    assert_eq!(
        std::fs::read_link(view.join("share")).unwrap(),
        checkout.join("share")
    );
    assert!(
        std::fs::symlink_metadata(view.join("libexec")).is_err(),
        "a directory the checkout lacks is not presented"
    );
    assert_eq!(
        std::fs::read_link(fx.seam("trust")).unwrap(),
        seam_target(&fx.layout, "trust"),
        "the rustup entry never moved"
    );
    assert!(
        reassert(&fx.layout, &fx.rustup).is_empty(),
        "the same checkout again is a quiet adoption"
    );
    assert_eq!(
        view_source(&fx.layout),
        ViewSource::Linked(checkout.clone()),
        "doctor compares the view against the checkout's tools"
    );
    let rebuilt = checkout.join("bin").join("trustc");
    std::fs::remove_file(&rebuilt).unwrap();
    std::fs::write(&rebuilt, "trustc, rebuilt").unwrap();
    assert_eq!(
        std::fs::read(crate::platform::resolve_shim(&bin.join("rustc")).unwrap()).unwrap(),
        b"trustc, rebuilt"
    );
    assert!(
        reassert(&fx.layout, &fx.rustup).is_empty(),
        "a rebuilt checkout needs no re-lay: the stubs name paths"
    );
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(store.join("bin").join("trustc"))
                .unwrap()
                .nlink(),
            1
        );
        assert_eq!(
            std::fs::read(store.join("bin").join("trustc")).unwrap(),
            b"trustc of build 6808"
        );
    }

    crate::linkmode::unlink(&fx.layout, "trust").unwrap();
    assert_eq!(view_source(&fx.layout), ViewSource::Store);
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(
            attached(&lines[0]),
            Some(Attached::Adopted { view_build: Some(b), .. }) if *b == store
        ),
        "{lines:?}"
    );
    assert!(cloned(
        &store.join("bin").join("trustc"),
        &bin.join("rustc")
    ));
    assert!(
        std::fs::symlink_metadata(view.join("lib"))
            .unwrap()
            .file_type()
            .is_dir(),
        "lib/ is a mirrored directory again, not a link"
    );
    assert!(
        std::fs::symlink_metadata(view.join("share")).is_err(),
        "the checkout's share/ went with it (the build ships none)"
    );
    assert!(
        std::fs::read_dir(&bin)
            .unwrap()
            .flatten()
            .all(|e| crate::platform::resolve_shim(&e.path()).is_none()),
        "no stub survives in the store's view: every entry is the store's own file"
    );
    assert!(reassert(&fx.layout, &fx.rustup).is_empty());
}

/// A dev-link to a checkout that is not a sysroot is refused and recorded.
#[cfg(unix)]
#[test]
fn a_dev_link_to_a_tree_that_is_no_sysroot_is_refused_and_recorded() {
    let fx = Fixture::new("dev-link-nosysroot");
    let store = fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let bin = view_dir(&fx.layout, "trust").join("bin");
    let proj = fx.root.path().join("proj");
    std::fs::create_dir_all(proj.join("bin")).unwrap();
    std::fs::write(proj.join("bin").join("trustc"), "a lone binary").unwrap();
    crate::linkmode::link(&fx.layout, "trust", &proj, &[PathBuf::from("bin/trustc")]).unwrap();
    assert_eq!(
        view_source(&fx.layout),
        ViewSource::LinkedNoSysroot(proj.clone())
    );
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(
            refusal(&lines[0]),
            Some(Refusal::LinkedNoSysroot { checkout }) if *checkout == proj
        ),
        "{lines:?}"
    );
    assert_eq!(
        refusals(&fx.layout).len(),
        1,
        "recorded for Settings and doctor"
    );
    assert!(
        cloned(&store.join("bin").join("trustc"), &bin.join("rustc")),
        "the view still presents the store"
    );
    assert_eq!(
        view_source(&fx.layout),
        ViewSource::LinkedNoSysroot(proj.clone()),
        "…and doctor compares it against the store"
    );
    crate::linkmode::unlink(&fx.layout, "trust").unwrap();
    assert!(
        reassert(&fx.layout, &fx.rustup).is_empty(),
        "a quiet adoption"
    );
    assert!(
        refusals(&fx.layout).is_empty(),
        "the refusal cleared with the link"
    );
}

/// A checkout inside atpkg's own prefix (its stubs would exec themselves) is refused as no
/// sysroot.
#[cfg(unix)]
#[test]
fn a_checkout_inside_the_prefix_is_refused_and_an_edited_stub_is_relaid() {
    let fx = Fixture::new("dev-link-shapes");
    fx.install_trust(6808);
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    crate::linkmode::link(&fx.layout, "trust", &view, &[PathBuf::from("bin/trustc")]).unwrap();
    assert_eq!(
        view_source(&fx.layout),
        ViewSource::LinkedNoSysroot(view.clone())
    );
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1
            && matches!(
                refusal(&lines[0]),
                Some(Refusal::LinkedNoSysroot { checkout }) if *checkout == view
            ),
        "{lines:?}"
    );
    assert!(
        std::fs::symlink_metadata(view.join("lib"))
            .unwrap()
            .file_type()
            .is_dir(),
        "the store's view stands untouched"
    );
    crate::linkmode::unlink(&fx.layout, "trust").unwrap();

    let checkout = sysroot_checkout(&fx, "stage2");
    crate::linkmode::link(
        &fx.layout,
        "trust",
        &checkout,
        &[PathBuf::from("bin/trustc")],
    )
    .unwrap();
    assert_eq!(reassert(&fx.layout, &fx.rustup).len(), 1);
    let stub = view.join("bin").join("rustc");
    let laid = std::fs::read(&stub).unwrap();
    std::fs::write(&stub, b"#!/bin/sh\nrm -rf something\nexec 'x'\n").unwrap();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1
            && matches!(
                attached(&lines[0]),
                Some(Attached::Adopted {
                    view_build: Some(_),
                    ..
                })
            ),
        "an edited stub is a changed view: {lines:?}"
    );
    assert_eq!(std::fs::read(&stub).unwrap(), laid, "re-laid byte for byte");
    assert!(reassert(&fx.layout, &fx.rustup).is_empty());
}

/// A dev-linked sysroot with no store build still gets the seam.
#[cfg(unix)]
#[test]
fn a_dev_linked_trust_with_no_store_build_still_gets_the_seam() {
    let fx = Fixture::new("dev-link-nostore");
    let checkout = sysroot_checkout(&fx, "stage2");
    crate::linkmode::link(
        &fx.layout,
        "trust",
        &checkout,
        &[PathBuf::from("bin/trustc")],
    )
    .unwrap();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        matches!(attached(&lines[0]), Some(Attached::Created { .. })),
        "{lines:?}"
    );
    let bin = view_dir(&fx.layout, "trust").join("bin");
    assert_eq!(
        crate::platform::resolve_shim(&bin.join("rustc")).unwrap(),
        checkout.join("bin").join("trustc")
    );
    assert_eq!(fx.seams_recorded(), vec!["rustup:trust".to_string()]);

    std::fs::remove_dir_all(checkout.join("lib")).unwrap();
    let lines = reassert(&fx.layout, &fx.rustup);
    assert!(
        lines.len() == 1 && matches!(refusal(&lines[0]), Some(Refusal::LinkedNoSysroot { .. })),
        "{lines:?}"
    );
    assert_eq!(refusals(&fx.layout).len(), 1);
    crate::linkmode::unlink(&fx.layout, "trust").unwrap();
    detach(&fx.layout, &fx.rustup, "trust", false).unwrap();
    assert!(
        refusals(&fx.layout).is_empty(),
        "the refusal went with the seam"
    );
    assert!(fx.seams_recorded().is_empty());
    assert!(std::fs::symlink_metadata(fx.seam("trust")).is_err());
}

/// A symlinked tool in the bundle's `bin/` is not presented.
#[cfg(unix)]
#[test]
fn a_symlinked_tool_in_the_bundle_is_left_out_of_the_view() {
    let fx = Fixture::new("view-symlink");
    let build = fx.install_trust(6808);
    std::fs::remove_file(build.join("bin").join("trustc")).unwrap();
    link(Path::new("targo"), &build.join("bin").join("trustc"));
    let r = refresh_view(&fx.layout, "trust").unwrap();
    assert_eq!(r.tools, 3);
    assert_eq!(r.stock, vec![("cargo", "targo"), ("rustdoc", "trustdoc")]);
    let bin = view_dir(&fx.layout, "trust").join("bin");
    assert!(std::fs::symlink_metadata(bin.join("trustc")).is_err());
    assert!(std::fs::symlink_metadata(bin.join("rustc")).is_err());
}

/// `(path, st_nlink, st_ctime, st_ctime_nsec)` of every entry under `dir`.
#[cfg(unix)]
fn inode_stamps(dir: &Path) -> Vec<(PathBuf, u64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            let m = std::fs::symlink_metadata(&path).unwrap();
            if m.is_dir() {
                stack.push(path.clone());
            }
            out.push((path, m.nlink(), m.ctime(), m.ctime_nsec()));
        }
    }
    out.sort();
    out
}

/// A second refresh of a matching view makes no link or unlink call.
#[cfg(unix)]
#[test]
fn a_second_refresh_of_a_matching_view_touches_no_store_inode() {
    let fx = Fixture::new("view-nochurn");
    let build = fx.install_trust(8595);
    std::fs::write(build.join("bin").join("rustc"), b"a second copy of trustc").unwrap();
    std::fs::create_dir_all(build.join("libexec")).unwrap();
    std::fs::write(build.join("libexec").join("helper"), b"helper").unwrap();
    attach(&fx.layout, &fx.rustup, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    assert!(view_matches(&build, &view, Depth::Deep));
    let store_before = inode_stamps(&build);
    let view_before = inode_stamps(&view);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let again = refresh_view(&fx.layout, "trust").unwrap();
    assert!(!again.changed);
    assert_eq!(again.tools, 5);
    assert_eq!(
        again.stock,
        vec![
            ("rustc", "trustc"),
            ("cargo", "targo"),
            ("rustdoc", "trustdoc")
        ],
        "the early return reports what a rebuild would have"
    );
    assert!(reassert(&fx.layout, &fx.rustup).is_empty());
    assert_eq!(inode_stamps(&build), store_before, "a store inode moved");
    assert_eq!(
        inode_stamps(&view),
        view_before,
        "the view's own entries moved"
    );
    let probe = build.join("bin").join("tippy");
    std::fs::hard_link(&probe, fx.root.path().join("probe-link")).unwrap();
    std::fs::remove_file(fx.root.path().join("probe-link")).unwrap();
    assert_ne!(inode_stamps(&build), store_before, "the stamps are blind");
}

/// `view_matches` catches a stock name hard-linked to its Trust tool (both depths).
#[cfg(unix)]
#[test]
fn view_matches_rejects_a_linked_or_rebytten_stock_name_and_a_planted_file() {
    let fx = Fixture::new("view-matches");
    let build = fx.install_trust(8595);
    let view = view_dir(&fx.layout, "trust");
    assert!(!view_matches(&build, &view, Depth::Shallow), "no view yet");
    refresh_view(&fx.layout, "trust").unwrap();
    assert!(view_matches(&build, &view, Depth::Shallow));
    assert!(view_matches(&build, &view, Depth::Deep));

    let rustc = view.join("bin").join("rustc");
    let trustc = build.join("bin").join("trustc");
    std::fs::remove_file(&rustc).unwrap();
    std::fs::hard_link(&trustc, &rustc).unwrap();
    assert!(!view_matches(&build, &view, Depth::Shallow));
    assert_eq!(
        first_mismatch(&build, &view, Depth::Deep),
        Some(rustc.clone())
    );
    refresh_view(&fx.layout, "trust").unwrap();
    assert!(cloned(&trustc, &rustc));
    assert!(view_matches(&build, &view, Depth::Deep));

    let mut bytes = std::fs::read(&trustc).unwrap();
    bytes[0] ^= 0x20;
    std::fs::remove_file(&rustc).unwrap();
    std::fs::write(&rustc, &bytes).unwrap();
    std::fs::set_permissions(&rustc, std::fs::metadata(&trustc).unwrap().permissions()).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&rustc)
        .unwrap()
        .set_modified(std::fs::metadata(&trustc).unwrap().modified().unwrap())
        .unwrap();
    assert!(
        view_matches(&build, &view, Depth::Shallow),
        "the attributes cannot see it"
    );
    assert_eq!(
        first_mismatch(&build, &view, Depth::Deep),
        Some(rustc.clone()),
        "the bytes can"
    );
    assert!(refresh_view(&fx.layout, "trust").unwrap().changed);
    assert!(cloned(&trustc, &rustc));
    assert!(view_matches(&build, &view, Depth::Deep));

    let planted = view.join("lib").join("rustlib").join("planted.dylib");
    std::fs::write(&planted, b"not the store's").unwrap();
    assert!(
        view_matches(&build, &view, Depth::Shallow),
        "shallow reads only lib/'s top level"
    );
    assert_eq!(
        first_mismatch(&build, &view, Depth::Deep),
        Some(planted.clone())
    );
    refresh_view(&fx.layout, "trust").unwrap();
    assert!(std::fs::symlink_metadata(&planted).is_err(), "re-mirrored");
    assert!(view_matches(&build, &view, Depth::Deep));

    let etc_link = view.join("lib").join("rustlib").join("etc-link");
    std::fs::remove_file(&etc_link).unwrap();
    link(Path::new("../../elsewhere"), &etc_link);
    assert_eq!(first_mismatch(&build, &view, Depth::Deep), Some(etc_link));
    refresh_view(&fx.layout, "trust").unwrap();

    let stray = view.join("bin").join("stray");
    std::fs::write(&stray, b"x").unwrap();
    assert_eq!(first_mismatch(&build, &view, Depth::Shallow), Some(stray));
    refresh_view(&fx.layout, "trust").unwrap();
    assert!(view_matches(&build, &view, Depth::Deep));

    std::fs::create_dir_all(view.join(".bin.old-1")).unwrap();
    assert!(view_matches(&build, &view, Depth::Deep));
}

/// Debris a killed rebuild left beside `bin/` (which `first_mismatch` ignores) is swept by the
/// next refresh.
#[cfg(unix)]
#[test]
fn a_killed_rebuilds_debris_is_swept_by_the_next_refresh() {
    let fx = Fixture::new("view-debris");
    let build = fx.install_trust(8595);
    refresh_view(&fx.layout, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    assert!(view_matches(&build, &view, Depth::Deep));

    let ours: Vec<PathBuf> = [
        ".bin.old-424242",
        ".bin.tmp-7",
        ".lib.old-424242",
        ".share.tmp-99",
    ]
    .iter()
    .map(|name| {
        let dir = view.join(name);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin").join("trustc"), b"a superseded build").unwrap();
        dir
    })
    .collect();
    let theirs: Vec<PathBuf> = [
        ".bin.old-notapid",
        ".bin.old-",
        ".notes.tmp-1",
        "bin.old-1",
        ".bin.keep-1",
    ]
    .iter()
    .map(|name| {
        let dir = view.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    })
    .collect();

    assert!(!refresh_view(&fx.layout, "trust").unwrap().changed);

    for gone in &ours {
        assert!(
            std::fs::symlink_metadata(gone).is_err(),
            "{} still holds a superseded build's clones",
            gone.display()
        );
    }
    for kept in &theirs {
        assert!(kept.is_dir(), "{} is not ours to delete", kept.display());
    }
    assert!(
        view_matches(&build, &view, Depth::Deep),
        "and the view it did not touch still stands"
    );
}

/// A `.DS_Store` under the view's `share/` is re-mirrored away without moving a `bin/` or store
/// `lib/` inode.
#[cfg(unix)]
#[test]
fn a_rebuild_relays_only_the_part_that_differs() {
    let fx = Fixture::new("view-parts");
    let build = fx.install_trust(8595);
    std::fs::create_dir_all(build.join("share")).unwrap();
    std::fs::write(build.join("share").join("README"), b"docs").unwrap();
    refresh_view(&fx.layout, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    assert!(view_matches(&build, &view, Depth::Deep));
    let store_bin = inode_stamps(&build.join("bin"));
    let store_lib = inode_stamps(&build.join("lib"));
    let view_bin = inode_stamps(&view.join("bin"));
    std::thread::sleep(std::time::Duration::from_millis(20));

    let ds_store = view.join("share").join(".DS_Store");
    std::fs::write(&ds_store, b"Finder").unwrap();
    let refreshed = refresh_view(&fx.layout, "trust").unwrap();
    assert!(!refreshed.changed, "bin/ did not change");
    assert!(std::fs::symlink_metadata(&ds_store).is_err());
    assert!(view_matches(&build, &view, Depth::Deep));
    assert_eq!(inode_stamps(&build.join("bin")), store_bin);
    assert_eq!(inode_stamps(&view.join("bin")), view_bin);
    assert_eq!(
        inode_stamps(&build.join("lib")),
        store_lib,
        "a share/ mismatch re-mirrored lib/"
    );

    if crate::platform::our_uid() != 0 {
        let locked = build.join("lib").join("rustlib").join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let first = refresh_view(&fx.layout, "trust");
        let second = refresh_view(&fx.layout, "trust");
        let (store_after, view_after) = (
            inode_stamps(&build.join("bin")),
            inode_stamps(&view.join("bin")),
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir(&locked).unwrap();
        assert!(first.is_err() && second.is_err(), "{first:?} {second:?}");
        assert_eq!(
            store_after, store_bin,
            "a failing mirror moved a store bin/ inode"
        );
        assert_eq!(view_after, view_bin);
    }

    std::fs::write(view.join("bin").join("stray"), b"x").unwrap();
    assert!(refresh_view(&fx.layout, "trust").unwrap().changed);
    assert!(view_matches(&build, &view, Depth::Deep));
}

/// A matching view is accepted only where [`Layout::ensure_dir`] would accept it.
#[cfg(unix)]
#[test]
fn a_matching_view_behind_a_link_or_a_loose_mode_is_not_accepted() {
    let fx = Fixture::new("view-failclosed");
    let build = fx.install_trust(8595);
    refresh_view(&fx.layout, "trust").unwrap();
    let view = view_dir(&fx.layout, "trust");
    let store = inode_stamps(&build);
    std::thread::sleep(std::time::Duration::from_millis(20));

    let views = views_root(&fx.layout);
    let aside = fx.root.path().join("views-aside");
    std::fs::rename(&views, &aside).unwrap();
    link(&aside, &views);
    assert!(
        view_matches(&build, &view, Depth::Deep),
        "fixture: whole behind the link"
    );
    let refused = refresh_view(&fx.layout, "trust");
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.to_string().contains("symlink")),
        "{refused:?}"
    );
    std::fs::remove_file(&views).unwrap();
    std::fs::rename(&aside, &views).unwrap();

    std::fs::set_permissions(&view, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(!refresh_view(&fx.layout, "trust").unwrap().changed);
    let mode = std::fs::symlink_metadata(view_tree(&view))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o700);
    assert_eq!(inode_stamps(&build), store, "a store inode moved");
}

#[cfg(unix)]
#[test]
fn refresh_view_and_stale_against_store_refuse_store_current_outside_trust_build() {
    let fx = Fixture::new("store-current-outside");
    let foreign = fx.root.path().join("foreign-trust");
    std::fs::create_dir_all(foreign.join("bin")).unwrap();
    std::fs::write(foreign.join("bin").join("trustc"), b"foreign").unwrap();

    let current = store_current(&fx.layout);
    std::fs::create_dir_all(current.parent().unwrap()).unwrap();
    link(&foreign, &current);

    let err = refresh_view(&fx.layout, "trust").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    assert_eq!(
        stale_against_store(&fx.layout, &fx.root.path().join("absent-sysroot")),
        None,
        "stale_against_store must refuse a store/trust/current pointing outside store/trust/<build>"
    );
}
