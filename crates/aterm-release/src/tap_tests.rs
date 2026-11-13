// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LIVE: &str =
    "cask \"aterm\" do\n  version \"0.63.0\"\n  sha256 \"fb16\"\n\n  url \"u\"\nend\n";

fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-tap-{label}-"))
        .tempdir()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::Git::new(dir).git(args).unwrap();
    assert!(out.success(), "git {args:?}: {}", out.stderr_utf8());
    out.stdout_utf8().trim().to_string()
}

fn source(root: &Path) -> PathBuf {
    let src = root.join("homebrew-tap");
    std::fs::create_dir_all(src.join("Casks")).unwrap();
    std::fs::write(src.join("README.md"), "# tap\n").unwrap();
    std::fs::write(src.join("MAINTENANCE.md"), "# maintenance\n").unwrap();
    std::fs::write(
        src.join("Casks/aterm.rb"),
        "cask \"aterm\" do\n  version \"0.1.0\"\n  sha256 \"0000\"\n\n  url \"new\"\nend\n",
    )
    .unwrap();
    std::fs::write(
        src.join("Casks/alab.rb"),
        "cask \"alab\" do\n  version \"0.1.0\"\n  sha256 \"0000\"\nend\n",
    )
    .unwrap();
    src
}

fn tap(root: &Path) -> PathBuf {
    let tap = root.join("tap");
    std::fs::create_dir_all(tap.join("Casks")).unwrap();
    crate::ledger::testing::init(
        &tap,
        &[
            ("user.name", "t"),
            ("user.email", "t@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    git(&tap, &["config", "core.hooksPath", "/dev/null"]);
    git(&tap, &["config", "core.excludesFile", "/dev/null"]);
    git(&tap, &["config", "core.attributesFile", "/dev/null"]);
    std::fs::write(tap.join("README.md"), "# old\n").unwrap();
    std::fs::write(tap.join("Casks/aterm.rb"), LIVE).unwrap();
    std::fs::write(tap.join("LICENSE"), "mine\n").unwrap();
    git(&tap, &["add", "-A"]);
    git(&tap, &["commit", "-q", "-m", "live"]);
    tap
}

#[test]
fn a_live_cask_keeps_its_version_and_sha256_and_nothing_else() {
    let src = "cask \"aterm\" do\n  version \"0.1.0\"\n  sha256 \"0000\"\n  url \"new\"\nend\n";
    assert_eq!(
        keep_live(src, LIVE).unwrap(),
        "cask \"aterm\" do\n  version \"0.63.0\"\n  sha256 \"fb16\"\n  url \"new\"\nend\n"
    );
    assert!(keep_live(src, "cask \"aterm\" do\nend\n").is_err());
    assert!(keep_live(src, "  version \"0.63.0\"\n  sha256 \"NOTHEX\"\n").is_err());
}

#[test]
fn a_deploy_mirrors_the_source_keeps_the_live_lines_and_commits_once() {
    let d = scratch("deploy");
    let (src, tap) = (source(d.path()), tap(d.path()));
    let before = git(&tap, &["rev-parse", "HEAD"]);
    let mut out = Vec::new();
    let dry = Options {
        tap: tap.clone(),
        dry_run: true,
    };
    let Deployed::Planned(planned) = deploy(&src, &dry, &mut out).unwrap() else {
        panic!("a dry run plans");
    };
    let said = String::from_utf8_lossy(&out);
    assert!(said.contains("seed   Casks/alab.rb"), "{said}");
    assert!(said.contains("update Casks/aterm.rb"), "{said}");
    assert!(said.contains("update README.md"), "{said}");
    assert_eq!(planned.len(), 4);
    assert_eq!(git(&tap, &["status", "--porcelain"]), "", "nothing written");
    assert_eq!(git(&tap, &["rev-parse", "HEAD"]), before);

    let real = Options {
        dry_run: false,
        ..dry
    };
    let Deployed::Committed(sha) = deploy(&src, &real, &mut Vec::new()).unwrap() else {
        panic!("a deploy commits");
    };
    assert_eq!(git(&tap, &["rev-parse", "--short", "HEAD"]), sha);
    assert_eq!(git(&tap, &["rev-list", "--count", "HEAD"]), "2");
    assert_eq!(git(&tap, &["log", "-1", "--format=%s"]), COMMIT_SUBJECT);
    assert_eq!(git(&tap, &["status", "--porcelain"]), "");
    let cask = std::fs::read_to_string(tap.join("Casks/aterm.rb")).unwrap();
    assert!(
        cask.contains("  version \"0.63.0\"\n  sha256 \"fb16\"\n"),
        "{cask}"
    );
    assert!(cask.contains("url \"new\""), "{cask}");
    assert_eq!(
        std::fs::read(tap.join("Casks/alab.rb")).unwrap(),
        std::fs::read(src.join("Casks/alab.rb")).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(tap.join("README.md")).unwrap(),
        "# tap\n"
    );
    assert_eq!(
        std::fs::read_to_string(tap.join("LICENSE")).unwrap(),
        "mine\n"
    );
    assert_eq!(
        git(&tap, &["remote"]),
        "",
        "no remote was added or pushed to"
    );

    assert_eq!(
        deploy(&src, &real, &mut Vec::new()).unwrap(),
        Deployed::Current
    );
    assert_eq!(git(&tap, &["rev-list", "--count", "HEAD"]), "2");
}

#[test]
fn a_workflow_internal_wording_or_no_checkout_is_refused() {
    let d = scratch("refuse");
    let (src, tap) = (source(d.path()), tap(d.path()));
    let real = Options {
        tap: tap.clone(),
        dry_run: false,
    };
    let not_a_checkout = Options {
        tap: d.path().join("nowhere"),
        dry_run: true,
    };
    assert!(deploy(&src, &not_a_checkout, &mut Vec::new()).is_err());
    std::fs::write(
        src.join("NOTES.md"),
        b"\xff Internal doc for the PRIVATE aterm DEV REPO.\n",
    )
    .unwrap();
    assert!(deploy(&src, &real, &mut Vec::new()).is_err());
    std::fs::remove_file(src.join("NOTES.md")).unwrap();
    std::fs::create_dir_all(src.join(".github/workflows")).unwrap();
    assert!(deploy(&src, &real, &mut Vec::new()).is_err());
    assert_eq!(
        git(&tap, &["rev-list", "--count", "HEAD"]),
        "1",
        "nothing committed"
    );
    assert_eq!(git(&tap, &["status", "--porcelain"]), "");
}

#[test]
fn a_staged_index_is_refused_and_nothing_else_rides_in_the_commit() {
    let d = scratch("staged");
    let (src, tap) = (source(d.path()), tap(d.path()));
    std::fs::write(tap.join("LICENSE"), "staged\n").unwrap();
    git(&tap, &["add", "LICENSE"]);
    for dry_run in [true, false] {
        let opts = Options {
            tap: tap.clone(),
            dry_run,
        };
        let refused = deploy(&src, &opts, &mut Vec::new()).unwrap_err();
        assert!(
            refused.contains("already holds staged changes"),
            "{refused}"
        );
    }
    assert_eq!(git(&tap, &["status", "--porcelain"]), "M  LICENSE");

    git(&tap, &["reset", "-q", "LICENSE"]);
    std::fs::write(tap.join("Casks/other.rb"), "cask \"other\" do\nend\n").unwrap();
    let opts = Options {
        tap: tap.clone(),
        dry_run: false,
    };
    let Deployed::Committed(_) = deploy(&src, &opts, &mut Vec::new()).unwrap() else {
        panic!("a deploy commits");
    };
    let committed = git(&tap, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(
        committed, "Casks/alab.rb\nCasks/aterm.rb\nMAINTENANCE.md\nREADME.md",
        "only what it planned"
    );
    assert_eq!(
        git(&tap, &["diff", "--name-only"]),
        "LICENSE",
        "the tap's own change is left as it was"
    );
    assert_eq!(
        git(&tap, &["ls-files", "--others", "--exclude-standard"]),
        "Casks/other.rb"
    );
}

#[test]
fn the_command_line_is_deploy_a_checkout_and_maybe_dry_run() {
    let p = |a: &[&str]| parse_args(a.iter().copied());
    assert_eq!(
        p(&["deploy", "/t", "--dry-run"]).unwrap(),
        Some(Options {
            tap: PathBuf::from("/t"),
            dry_run: true
        })
    );
    assert_eq!(p(&["--help"]).unwrap(), None);
    assert!(p(&["deploy"]).is_err());
    assert!(p(&["deploy", "/a", "/b"]).is_err());
    assert!(p(&["push", "/a"]).is_err());
    assert!(p(&["deploy", "/a", "--force"]).is_err());
}

#[cfg(unix)]
#[test]
fn tap_planning_refuses_aliases_in_every_published_source() {
    for rel in ["Casks/aterm.rb", "README.md", "MAINTENANCE.md", "Casks"] {
        let dir = scratch("file-admission");
        let src = source(dir.path());
        let destination = dir.path().join("empty-tap");
        std::fs::create_dir(&destination).unwrap();
        assert_eq!(plan(&src, &destination).unwrap().len(), 4);

        let cask = src.join("Casks/aterm.rb");
        let public = std::fs::read(&cask).unwrap();
        let internal = b"# private dev repo\ncask \"aterm\" do\nend\n";
        std::fs::write(&cask, internal).unwrap();
        assert!(
            plan(&src, &destination).is_err(),
            "regular internal wording is refused"
        );
        std::fs::write(&cask, &public).unwrap();

        let selected = src.join(rel);
        let outside = dir.path().join("outside-source");
        std::fs::rename(&selected, &outside).unwrap();
        let private = if rel == "Casks" {
            outside.join("aterm.rb")
        } else {
            outside.clone()
        };
        std::fs::write(&private, internal).unwrap();
        std::os::unix::fs::symlink(&outside, &selected).unwrap();
        assert!(
            plan(&src, &destination).is_err(),
            "unadmitted source {rel} must not enter a publication plan"
        );
        assert_eq!(std::fs::read(&private).unwrap(), internal);
        assert!(std::fs::read_dir(&destination).unwrap().next().is_none());

        // Restore valid source and verify that symlinks in the destination tap are also refused.
        std::fs::remove_file(&selected).unwrap();
        std::fs::rename(&outside, &selected).unwrap();
        std::fs::write(&cask, &public).unwrap();
        let tap_dir = tap(dir.path());
        let tap_target = tap_dir.join(rel);
        let outside_tap = dir.path().join("outside-tap");
        if tap_target.is_dir() {
            std::fs::rename(&tap_target, &outside_tap).unwrap();
        } else {
            let _ = std::fs::remove_file(&tap_target);
            std::fs::write(&outside_tap, "outside\n").unwrap();
        }
        std::os::unix::fs::symlink(&outside_tap, &tap_target).unwrap();
        assert!(
            plan(&src, &tap_dir).is_err(),
            "symlink at destination {rel} must be refused"
        );
        let tap_link = dir.path().join("tap-root-link");
        std::os::unix::fs::symlink(&destination, &tap_link).unwrap();
        assert!(
            plan(&src, &tap_link).is_err(),
            "symlinked tap root must be refused"
        );
    }
}
