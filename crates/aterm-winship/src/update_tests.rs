// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn layout(root: &Path) -> Layout {
    Layout {
        local: root.to_path_buf(),
    }
}

#[test]
fn layout_keeps_everything_under_one_local_app_data() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let env = vec![(
        "LocalAppData".to_string(),
        scratch.path().to_string_lossy().into_owned(),
    )];
    let l = Layout::from_env(&env).expect("LOCALAPPDATA");
    for path in [
        l.src(),
        l.tool(),
        l.dist("1a37de31a340"),
        l.latest(),
        l.log(),
        l.smoke_report("1a37de31a340"),
        l.installed(),
        l.tree_tool_dir(),
    ] {
        assert!(path.starts_with(scratch.path()), "{}", path.display());
    }
    assert!(
        l.src()
            .ends_with(Path::new("aterm").join("build").join("src"))
    );
    assert!(
        l.latest()
            .ends_with(Path::new("aterm").join("dist").join("latest.json"))
    );
    assert!(
        l.dist("1a37de31a340")
            .ends_with(Path::new("aterm").join("dist").join("1a37de31a340"))
    );
    assert!(l.installed().ends_with(Path::new("Programs").join("aterm")));
    // Negative controls: no LOCALAPPDATA, or a relative one, is refused.
    assert!(Layout::from_env(&[]).is_err());
    assert!(Layout::from_env(&[("LOCALAPPDATA".to_string(), "relative".to_string())]).is_err());
}

#[test]
fn sweep_removes_old_dists_and_their_reports_and_nothing_else() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let l = layout(scratch.path());
    let root = l.dist_root();
    for dist in ["aaaaaaaaaaaa", "bbbbbbbbbbbb", "cccccccccccc"] {
        std::fs::create_dir_all(root.join(dist)).expect("dist");
        std::fs::write(root.join(dist).join("aterm.exe"), b"image").expect("image");
        std::fs::write(l.smoke_report(dist), b"PASS\n").expect("report");
    }
    std::fs::create_dir_all(root.join("notes")).expect("a person's folder");
    std::fs::write(l.latest(), b"{}").expect("pointer");
    // A file with a dist's name is not a dist folder.
    std::fs::write(root.join("dddddddddddd"), b"file").expect("file");

    let mut out = Out::terminal();
    sweep(&l, &["aaaaaaaaaaaa", "bbbbbbbbbbbb"], &mut out);

    assert!(!root.join("cccccccccccc").exists(), "the old dist went");
    assert!(!l.smoke_report("cccccccccccc").exists(), "and its report");
    for kept in ["aaaaaaaaaaaa", "bbbbbbbbbbbb"] {
        assert!(
            root.join(kept).join("aterm.exe").is_file(),
            "{kept} was kept"
        );
        assert!(l.smoke_report(kept).is_file(), "{kept}'s report was kept");
    }
    assert!(root.join("notes").is_dir());
    assert!(l.latest().is_file());
    assert!(root.join("dddddddddddd").is_file());
}

#[test]
fn a_logged_run_appends_its_lines() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let l = layout(scratch.path());
    for text in ["update: first", "update: second"] {
        let mut out = Out::logged(&l.log()).expect("log");
        out.say(text);
    }
    let log = std::fs::read_to_string(l.log()).expect("log");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(
        lines
            .first()
            .is_some_and(|l| l.ends_with("Z update: first")),
        "{log}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|l| l.ends_with("Z update: second")),
        "{log}"
    );
}

#[test]
fn sweep_removes_what_an_interrupted_run_left() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let l = layout(scratch.path());
    let root = l.dist_root();
    for dir in [
        "aaaaaaaaaaaa",
        "aaaaaaaaaaaa.building",
        "bbbbbbbbbbbb.old",
        "notes.building",
    ] {
        std::fs::create_dir_all(root.join(dir)).expect("dir");
        std::fs::write(root.join(dir).join("aterm.exe"), b"image").expect("image");
    }
    sweep(&l, &["aaaaaaaaaaaa"], &mut Out::terminal());
    assert!(!l.building("aaaaaaaaaaaa").exists());
    assert!(!l.replaced("bbbbbbbbbbbb").exists());
    // Controls: the kept dist and a folder of another shape stay.
    assert!(root.join("aaaaaaaaaaaa").join("aterm.exe").is_file());
    assert!(root.join("notes.building").is_dir());
}

#[test]
fn the_installer_is_never_asked_to_remake_a_first_install() {
    assert_eq!(installer_args(false, true), ["-Staged", "-Update"]);
    assert_eq!(
        installer_args(true, true),
        ["-Staged", "-Restart", "-Update"]
    );
    // Controls: a first install lays its shortcuts and PATH entry once.
    assert_eq!(installer_args(false, false), ["-Staged"]);
    assert_eq!(installer_args(true, false), ["-Staged", "-Restart"]);
}

const HEAD: &str = "aaaaaaaaaaaa0000000000000000000000000000";
const OTHER: &str = "bbbbbbbbbbbb0000000000000000000000000000";

fn build_json(full: &str, number: u64) -> String {
    format!(
        "{{\"schema\":1,\"version\":\"0.199.0\",\"commit\":\"{}\",\"commit_full\":\"{full}\",\"build_number\":{number},\"arch\":\"x64\",\"toolchain\":\"stock 1.97.1\",\"dirty\":false,\"built_at\":\"2026-11-11T19:14:22Z\"}}\n",
        &full[..12]
    )
}

/// A folder that looks like a dist of `full`, its image holding `mark`.
fn fill(dir: &Path, full: &str, number: u64, mark: &str) {
    std::fs::create_dir_all(dir).expect("dist");
    std::fs::write(dir.join("aterm.exe"), mark).expect("image");
    std::fs::write(dir.join("BUILD.json"), build_json(full, number)).expect("record");
}

fn image(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("aterm.exe")).unwrap_or_else(|e| format!("({e})"))
}

/// A layout whose pointer names a staged, smoked dist of [`HEAD`] holding `old`.
fn staged_layout(root: &Path) -> (Layout, Pointer) {
    let l = layout(root);
    let dist = l.dist(&HEAD[..12]);
    fill(&dist, HEAD, 100, "old");
    let record = Build::read(&dist).expect("parses").expect("is there");
    let pointer = Pointer::of(
        &record,
        &dist,
        "2026-11-11T19:20:00Z".to_string(),
        Smoke::Pass,
    );
    pointer.write(&l.latest()).expect("pointer");
    (l, pointer)
}

fn never_smoked(_: &Path, _: &mut Out) -> (Smoke, String) {
    panic!("a build that failed was smoked")
}

/// Whatever way the build fails, the dist the pointer names and the pointer
/// are as they were, during the build and after it, and nothing half-made is
/// left under a name a reader would take for a dist.
#[test]
fn a_failed_rebuild_leaves_the_staged_build_and_the_pointer_as_they_were() {
    type Fails = fn(&Path) -> Result<Option<i32>, String>;
    let fails: [(&str, Fails); 5] = [
        ("build.ps1 exited 1", |to| {
            std::fs::create_dir_all(to).expect("dir");
            std::fs::write(to.join("aterm.exe"), "half").expect("image");
            Ok(Some(1))
        }),
        ("was ended without an exit code", |to| {
            fill(to, HEAD, 100, "half");
            Ok(None)
        }),
        ("wrote no BUILD.json", |to| {
            std::fs::create_dir_all(to).expect("dir");
            std::fs::write(to.join("aterm.exe"), "half").expect("image");
            Ok(Some(0))
        }),
        ("BUILD.json names commit", |to| {
            fill(to, OTHER, 100, "another");
            Ok(Some(0))
        }),
        ("powershell.exe: not found", |_| {
            Err("powershell.exe: not found".to_string())
        }),
    ];
    for (why, fail) in fails {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let (l, pointer) = staged_layout(scratch.path());
        let dist = l.dist(&HEAD[..12]);
        let mut out = Out::terminal();
        let failed = stage_built(
            &l,
            HEAD,
            Some(&pointer),
            &mut out,
            |to, _| {
                // While the build runs, a reader still finds the old build whole.
                assert_eq!(to, l.building(&HEAD[..12]));
                assert_eq!(image(&dist), "old", "{why}: during the build");
                assert_eq!(Pointer::read(&l.latest()), Ok(Some(pointer.clone())));
                fail(to)
            },
            never_smoked,
        )
        .expect_err(why);
        assert_eq!(failed.0, FAILED, "{why}");
        assert!(failed.1.contains(why), "{why}: {}", failed.1);
        assert!(failed.1.contains("as they were"), "{why}: {}", failed.1);
        assert_eq!(image(&dist), "old", "{why}: the staged dist");
        assert_eq!(
            Pointer::read(&l.latest()),
            Ok(Some(pointer.clone())),
            "{why}: the pointer"
        );
        assert!(
            !l.building(&HEAD[..12]).exists(),
            "{why}: the half-made folder"
        );
        assert!(!l.replaced(&HEAD[..12]).exists(), "{why}");
    }
}

/// The control of the test above: a build that is whole replaces the dist,
/// and the pointer that named the replaced folder is down until the smoke of
/// the new one has a verdict.
#[test]
fn a_rebuild_takes_the_name_once_it_is_whole_and_is_staged_after_its_smoke() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let (l, pointer) = staged_layout(scratch.path());
    let dist = l.dist(&HEAD[..12]);
    // What an interrupted run left is not built into.
    std::fs::create_dir_all(l.building(&HEAD[..12])).expect("dir");
    std::fs::write(l.building(&HEAD[..12]).join("stale"), "x").expect("file");
    let mut out = Out::terminal();
    let staged = stage_built(
        &l,
        HEAD,
        Some(&pointer),
        &mut out,
        |to, _| {
            assert!(!to.exists(), "the stale folder was not cleared");
            fill(to, HEAD, 100, "new");
            Ok(Some(0))
        },
        |smoked, _| {
            assert_eq!(smoked, dist);
            assert_eq!(image(smoked), "new", "the smoke judges the new build");
            assert_eq!(
                Pointer::read(&l.latest()),
                Ok(None),
                "no pointer names the folder while its verdict is the old build's"
            );
            assert!(!l.building(&HEAD[..12]).exists());
            assert!(!l.replaced(&HEAD[..12]).exists());
            (Smoke::Fail, "FAIL shell: no answer\n".to_string())
        },
    )
    .expect("staged");
    assert_eq!(staged.smoke, Smoke::Fail);
    assert_eq!(staged.dir, dist);
    assert_eq!(Pointer::read(&l.latest()), Ok(Some(staged)));
    assert_eq!(image(&dist), "new");
    assert_eq!(
        std::fs::read_to_string(l.smoke_report(&HEAD[..12])).expect("report"),
        "FAIL shell: no answer\n"
    );
}

/// A pointer that names another dist stays up through the build and the
/// smoke: the window keeps offering the previous build until the new one is
/// staged, and the previous dist is kept.
#[test]
fn a_new_commit_is_staged_beside_the_previous_one() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let (l, pointer) = staged_layout(scratch.path());
    let mut out = Out::terminal();
    let staged = stage_built(
        &l,
        OTHER,
        Some(&pointer),
        &mut out,
        |to, _| {
            fill(to, OTHER, 200, "next");
            Ok(Some(0))
        },
        |_, _| {
            assert_eq!(Pointer::read(&l.latest()), Ok(Some(pointer.clone())));
            (Smoke::Pass, String::new())
        },
    )
    .expect("staged");
    assert_eq!(
        (staged.commit.as_str(), staged.build_number, staged.smoke),
        (&OTHER[..12], 200, Smoke::Pass)
    );
    assert_eq!(image(&l.dist(&OTHER[..12])), "next");
    assert_eq!(
        image(&l.dist(&HEAD[..12])),
        "old",
        "the previous dist is kept"
    );
}

#[test]
fn a_dist_that_cannot_be_replaced_is_put_back() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let l = layout(scratch.path());
    let (dist, building, replaced) = (
        l.dist(&HEAD[..12]),
        l.building(&HEAD[..12]),
        l.replaced(&HEAD[..12]),
    );
    fill(&dist, HEAD, 100, "old");
    // Nothing was built: the folder that should take the name is not there.
    let refused = swap_in(&building, &dist, &replaced).expect_err("nothing to name");
    assert!(refused.contains("could not be named"), "{refused}");
    assert_eq!(image(&dist), "old", "the dist is back under its name");
    assert!(!replaced.exists());
    // Control: with a finished folder the swap is taken and nothing is left aside.
    fill(&building, HEAD, 100, "new");
    swap_in(&building, &dist, &replaced).expect("swapped");
    assert_eq!(image(&dist), "new");
    assert!(!building.exists() && !replaced.exists());
}

#[test]
fn a_smoke_s_exit_code_is_its_verdict() {
    assert_eq!(smoke_verdict(Some(PASS)), Smoke::Pass);
    assert_eq!(smoke_verdict(Some(FAILED)), Smoke::Fail);
    assert_eq!(smoke_verdict(Some(NOT_RUN)), Smoke::NotRun);
    // A usage error, a crash or a kill judged nothing good: the build is not passed.
    assert_eq!(smoke_verdict(Some(crate::USAGE)), Smoke::Fail);
    assert_eq!(smoke_verdict(Some(-1073741819)), Smoke::Fail);
    assert_eq!(smoke_verdict(None), Smoke::Fail);
}

#[test]
fn the_tree_s_tool_replaces_the_scheduled_copy_and_keeps_the_old_one_aside() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let fresh = scratch.path().join("tool").join("aterm-winship.exe");
    std::fs::create_dir_all(fresh.parent().expect("parent")).expect("mkdir");
    std::fs::write(&fresh, b"new tool").expect("write");
    let kept = scratch.path().join("aterm-winship.exe");

    // No scheduled copy: nothing is made (only `schedule` makes one).
    assert_eq!(adopt_tool(&fresh, &kept), Ok(false));
    assert!(!kept.exists());

    // The same bytes: nothing is touched.
    std::fs::write(&kept, b"new tool").expect("write");
    assert_eq!(adopt_tool(&fresh, &kept), Ok(false));
    assert!(!old_tool(&kept).exists());

    // Other bytes: the old copy goes aside, the new one takes its name.
    std::fs::write(&kept, b"old tool").expect("write");
    std::fs::write(old_tool(&kept), b"older still").expect("write");
    assert_eq!(adopt_tool(&fresh, &kept), Ok(true));
    assert_eq!(std::fs::read(&kept).expect("read"), b"new tool");
    assert_eq!(std::fs::read(old_tool(&kept)).expect("read"), b"old tool");
    assert_eq!(
        old_tool(&kept).file_name().and_then(|n| n.to_str()),
        Some("aterm-winship.exe.old")
    );

    // A fresh tool that is not there: an error, and the scheduled copy is as it was.
    let gone = scratch.path().join("gone.exe");
    assert!(adopt_tool(&gone, &kept).is_err());
    assert_eq!(std::fs::read(&kept).expect("read"), b"new tool");
}

#[test]
fn only_a_tool_that_reached_its_own_verdict_is_kept() {
    assert!(reached_verdict(Some(PASS)));
    assert!(reached_verdict(Some(FAILED)));
    // Not run, a usage error, a panic, a missing DLL, a kill: the tool proved nothing.
    for code in [
        Some(NOT_RUN),
        Some(crate::USAGE),
        Some(101),
        Some(-1073741515),
        None,
    ] {
        assert!(!reached_verdict(code), "{code:?}");
    }
}

#[test]
fn the_tool_flag_is_passed_only_to_a_build_ps1_that_declares_it() {
    let now = std::fs::read_to_string(
        aterm_tempfile::manifest_dir()
            .join("..")
            .join("..")
            .join("apps")
            .join("aterm-win")
            .join("build.ps1"),
    )
    .expect("build.ps1");
    assert!(takes_tool(&now));
    // A tree from before -Tool: `-Toolchain` alone, which PowerShell would match `-Tool` to.
    let before = "param(\n    [string]$Out,\n    [ValidateSet(\"auto\", \"trust\", \"stock\")]\n    [string]$Toolchain = \"auto\",\n    [string]$Builder,\n    [switch]$AllowDirty\n)\n";
    assert!(!takes_tool(before));
    assert!(takes_tool(
        &before.replace("$AllowDirty\n", "$AllowDirty,\n    [string]$Tool\n")
    ));
}
