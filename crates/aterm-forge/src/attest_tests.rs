// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::pristine::tests::{Served, crate_file};

/// [`report`] with `[OB-7]`'s pristine copies from `served`, cached in a folder of its own.
fn report_served(root: &Path, served: Served) -> Attestation {
    let cache = aterm_tempfile::tempdir().unwrap();
    let pristine = Pristine::at(Some(cache.path().to_path_buf()), Box::new(served));
    report_using(&crate::resolve::Cargo::from_env(), &pristine, root)
}

/// The absorbed fixtures' upstream manifest.
const ABSORBED_MANIFEST: &str =
    "[package]\nname = \"absorbed\"\nversion = \"2.0.0\"\nlicense = \"Apache-2.0\"\n";

/// crates.io's `absorbed 2.0.0`, the upstream the absorbed fixtures diverge from.
fn absorbed_upstream() -> Served {
    let archive = crate_file(
        "absorbed-2.0.0",
        &[
            ("Cargo.toml", ABSORBED_MANIFEST.as_bytes()),
            ("LICENSE", b"Apache\n"),
            ("src/lib.rs", b"pub fn upstream() {}\n"),
        ],
    );
    Served::honest("absorbed", "2.0.0", archive)
}

#[test]
fn apache_is_only_mandatory_when_no_other_arm_exists() {
    assert!(apache_is_mandatory("Apache-2.0"));
    assert!(apache_is_mandatory("Apache-2.0 AND MIT"));
    assert!(apache_is_mandatory("Apache-2.0 AND (MIT OR BSD-3-Clause)"));
    assert!(apache_is_mandatory(
        "(Apache-2.0 AND MIT) OR (Apache-2.0 AND BSD-3-Clause)"
    ));
    assert!(apache_is_mandatory(
        "Apache-2.0 WITH LLVM-exception AND (MIT OR Zlib)"
    ));
    assert!(!apache_is_mandatory("Apache-2.0 OR MIT"));
    assert!(!apache_is_mandatory("MIT OR Apache-2.0"));
    assert!(!apache_is_mandatory("(Apache-2.0 OR MIT) AND BSD-3-Clause"));
    assert!(!apache_is_mandatory("MIT"));
    assert!(!apache_is_mandatory(""));
}

#[test]
fn license_arms_unfold_or_and_and_with() {
    assert_eq!(license_arms("MIT OR Apache-2.0"), ["MIT", "Apache-2.0"]);
    assert_eq!(
        license_arms("(MIT OR Apache-2.0) AND Unicode-3.0"),
        ["MIT", "Apache-2.0", "Unicode-3.0"]
    );
    assert_eq!(
        license_arms("Apache-2.0 WITH LLVM-exception OR MIT"),
        ["Apache-2.0", "MIT"]
    );
}

#[test]
fn caret_requirements_follow_cargos_zero_major_rules() {
    assert_eq!(req_satisfied("0.30", "0.30.13"), Some(true));
    assert_eq!(req_satisfied("0.30", "0.31.0"), Some(false));
    assert_eq!(req_satisfied("^0.30.5", "0.30.13"), Some(true));
    assert_eq!(req_satisfied("1", "1.9.0"), Some(true));
    assert_eq!(req_satisfied("1", "2.0.0"), Some(false));
    assert_eq!(req_satisfied("=0.33.1", "0.33.1"), Some(true));
    assert_eq!(req_satisfied("=0.33.1", "0.33.2"), Some(false));
    assert_eq!(req_satisfied(">=1, <3", "2.0.0"), None);
    assert!(parse_version("1.2.3.4").is_none());
    assert_eq!(req_satisfied("1.2.3.4", "1.2.3"), None);
    assert_eq!(req_satisfied("1.2.3", "1.2.3.4"), None);
    let max = u64::MAX;
    assert_eq!(
        req_satisfied(&format!("^{max}.0.0"), &format!("{max}.{max}.{max}")),
        Some(true)
    );
    assert_eq!(
        req_satisfied(&format!("^0.{max}.0"), &format!("0.{max}.{max}")),
        Some(true)
    );
    assert_eq!(
        req_satisfied(&format!("^0.0.{max}"), &format!("0.0.{max}")),
        Some(true)
    );
}

#[test]
fn notice_lines_parse_into_name_version_license_and_dir() {
    let entries = parse_notice(
        "prose\n- libm 0.2.16, MIT (`vendor/libm/`)\n- DejaVu: x\n\
         - winit 0.30.13, Apache-2.0 (`crates/aterm-window/`)\n- elsewhere 1.0, MIT (`x/y/`)\n",
    );
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0].name, "libm");
    assert_eq!(entries[0].version, "0.2.16");
    assert_eq!(entries[0].license, "MIT");
    assert_eq!(entries[0].dir, "vendor/libm");
    assert_eq!(entries[0].line, 2);
    assert_eq!(entries[1].name, "winit");
    assert_eq!(entries[1].dir, "crates/aterm-window");
    assert_eq!(entries[1].line, 4);
}

#[test]
fn a_header_notice_is_recognized_and_a_buried_one_is_not() {
    assert!(has_modification_notice(
        "// Modified by aterm: pump WM_TIMER.\nfn main() {}"
    ));
    assert!(!has_modification_notice("use std::ptr;\nfn main() {}"));
    let buried = format!(
        "{}// LOCAL PATCH (aterm): late\n",
        "\n".repeat(HEADER_LINES + 1)
    );
    assert!(
        !has_modification_notice(&buried),
        "§4(b) wants a PROMINENT notice"
    );
}

struct Fixture(aterm_tempfile::TempDir);

fn good_fixture(tag: &str) -> Fixture {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-forge-attest-{tag}-"))
        .tempdir()
        .unwrap();
    let fork = dir.path().join("vendor/goodfork/src");
    std::fs::create_dir_all(&fork).unwrap();
    let w = |rel: &str, body: &str| std::fs::write(dir.path().join(rel), body).unwrap();
    w(
        "Cargo.toml",
        "[workspace]\nmembers = []\n\n[patch.crates-io]\ngoodfork = { path = \"vendor/goodfork\" }\n",
    );
    w(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"goodfork\"\nversion = \"1.2.3\"\n",
    );
    w("deny.toml", "[licenses]\nallow = [\"MIT\"]\n");
    w(
        "NOTICE",
        "prose\n\n- goodfork 1.2.3, MIT (`vendor/goodfork/`)\n\nmore prose\n",
    );
    w(
        "vendor/goodfork/Cargo.toml",
        "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"MIT\"\n\n[workspace]\n",
    );
    w(
        "vendor/goodfork/Cargo.toml.orig",
        "[package]\nname = \"goodfork\"\n",
    );
    w(
        "vendor/goodfork/.cargo_vcs_info.json",
        "{\"git\":{\"sha1\":\"deadbeef\"}}\n",
    );
    w("vendor/goodfork/LICENSE-MIT", "MIT\n");
    w(
        "vendor/goodfork/src/lib.rs",
        "// aterm-trust: bounds proof discharged here.\n",
    );
    Fixture(dir)
}

#[test]
fn every_planted_fork_defect_reds_its_own_obligation() {
    let Attestation { ok, log, .. } = report(good_fixture("green").0.path());
    assert!(
        ok,
        "the baseline must attest clean or the plants prove nothing:\n{log}"
    );
    let cases: [(&str, &str, &str, &str); 5] = [
        (
            "a NOTICE that over-claims",
            "NOTICE",
            "- goodfork 1.2.3, MIT (`vendor/goodfork/`)\n- ghost 9.9.9, MIT (`vendor/ghost/`)\n",
            "FAIL [OB-6]",
        ),
        (
            "no workspace stub",
            "vendor/goodfork/Cargo.toml",
            "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"MIT\"\n",
            "FAIL [OB-3]",
        ),
        (
            "a license off the allowlist",
            "vendor/goodfork/Cargo.toml",
            "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"GPL-3.0\"\n\n[workspace]\n",
            "FAIL [OB-9]",
        ),
        (
            "a fork with no marker",
            "vendor/goodfork/src/lib.rs",
            "pub fn f() {}\n",
            "FAIL [OB-8]",
        ),
        (
            "a patch the lock resolves from the registry",
            "Cargo.lock",
            "version = 4\n\n[[package]]\nname = \"goodfork\"\nversion = \"1.2.3\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
            "FAIL [OB-2]",
        ),
    ];
    for (name, rel, body, tag) in cases {
        let fixture = good_fixture(&name.replace(' ', "-"));
        std::fs::write(fixture.0.path().join(rel), body).unwrap();
        let Attestation { ok, log, .. } = report(fixture.0.path());
        assert!(!ok && log.contains(tag), "{name} must fail {tag}:\n{log}");
    }
    let fixture = good_fixture("dir-provenance");
    std::fs::remove_file(fixture.0.path().join("vendor/goodfork/Cargo.toml.orig")).unwrap();
    std::fs::create_dir(fixture.0.path().join("vendor/goodfork/Cargo.toml.orig")).unwrap();
    let Attestation { ok, log, .. } = report(fixture.0.path());
    assert!(!ok && log.contains("FAIL [OB-4]"), "{log}");

    let fixture = good_fixture("dir-license");
    std::fs::remove_file(fixture.0.path().join("vendor/goodfork/LICENSE-MIT")).unwrap();
    std::fs::create_dir(fixture.0.path().join("vendor/goodfork/LICENSE-MIT")).unwrap();
    let Attestation { ok, log, .. } = report(fixture.0.path());
    assert!(!ok && log.contains("FAIL [OB-5]"), "{log}");
}

#[test]
fn renamed_forks_still_refuse_unmodelled_workspace_requirements() {
    let fixture = good_fixture("renamed-requirements");
    let root = fixture.0.path();
    let require = |req: &str| {
        std::fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nmembers = []\n\n[workspace.dependencies]\ngoodfork = \
                 \"{req}\"\n\n[patch.crates-io]\ngoodfork-v1 = {{ package = \
                 \"goodfork\", path = \"vendor/goodfork\" }}\n"
            ),
        )
        .unwrap();
    };
    require("2");
    let Attestation { ok, log, .. } = report(root);
    assert!(ok, "a valid other-major requirement stays excluded:\n{log}");

    let mut accepted = Vec::new();
    for req in ["*", ">=0, <3", ">2", "0.*", "~2"] {
        require(req);
        let Attestation { ok, log, .. } = report(root);
        if ok || !log.contains("requirement form attest does not model") {
            accepted.push((req, log));
        }
    }
    assert!(
        accepted.is_empty(),
        "renaming a row must not bypass the requirement reader:\n{accepted:#?}"
    );

    // Wide zero-major requirements (`0`, `0.0`) span multiple semver lines and stay in scope,
    // and renamed or target-gated dependencies (`alias = { package = "goodfork", ... }`) are read too.
    for req in ["0", "0.0"] {
        require(req);
        let Attestation { ok, log, .. } = report(root);
        assert!(
            !ok && log.contains("FAIL [OB-2]"),
            "`{req}` must not be dropped out of scope:\n{log}"
        );
    }
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/consumer\"]\n\n[patch.crates-io]\ngoodfork = { path = \"vendor/goodfork\" }\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("crates/consumer")).unwrap();
    std::fs::write(
        root.join("crates/consumer/Cargo.toml"),
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n[target.'cfg(unix)'.dependencies]\nalias = { package = \"goodfork\", version = \"2.0\" }\n",
    )
    .unwrap();
    let Attestation { ok, log, .. } = report(root);
    assert!(
        !ok && log.contains("FAIL [OB-2]"),
        "renamed target-gated dependency must be checked by OB-2:\n{log}"
    );
}

fn absorbed_fixture(tag: &str, renamed: bool) -> Fixture {
    let fixture = good_fixture(tag);
    let root = fixture.0.path().to_path_buf();
    let w = |rel: &str, body: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    let append = |rel: &str, extra: &str| {
        let path = root.join(rel);
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(extra);
        std::fs::write(path, body).unwrap();
    };
    if !renamed {
        append(
            "Cargo.toml",
            "absorbed = { path = \"crates/aterm-absorbed\" }\n",
        );
        append(
            "Cargo.lock",
            "\n[[package]]\nname = \"absorbed\"\nversion = \"2.0.0\"\n",
        );
    }
    append(
        "NOTICE",
        "- absorbed 2.0.0, Apache-2.0 (`crates/aterm-absorbed/`)\n",
    );
    w(
        "deny.toml",
        "[licenses]\nallow = [\"MIT\", \"Apache-2.0\"]\n",
    );
    if renamed {
        w(
            "crates/aterm-absorbed/Cargo.toml",
            "# Modified by the aterm project: renamed.\n[package]\nname = \"aterm-absorbed\"\n\
             version = \"2.0.0\"\nlicense = \"Apache-2.0\"\n",
        );
    } else {
        w("crates/aterm-absorbed/Cargo.toml", ABSORBED_MANIFEST);
    }
    w("crates/aterm-absorbed/LICENSE", "Apache\n");
    w(
        "crates/aterm-absorbed/src/lib.rs",
        "// Derived from absorbed 2.0.0 src/lib.rs, modified by the aterm project.\n\
         pub fn upstream() {}\n",
    );
    w(
        "crates/aterm-absorbed/src/added.rs",
        "// Added by the aterm project.\npub fn ours() {}\n",
    );
    fixture
}

#[test]
fn an_absorbed_crate_owes_the_license_obligations() {
    let fixture = absorbed_fixture("absorbed-green", false);
    let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(ok, "a complete absorbed crate must attest clean:\n{log}");

    let fixture = absorbed_fixture("absorbed-no-notice", false);
    std::fs::write(
        fixture.0.path().join("crates/aterm-absorbed/src/lib.rs"),
        "pub fn upstream() { /* edited */ }\n",
    )
    .unwrap();
    let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(!ok && log.contains("FAIL [OB-7]"), "{log}");

    let fixture = absorbed_fixture("absorbed-no-notice-line", false);
    let notice = fixture.0.path().join("NOTICE");
    let text = std::fs::read_to_string(&notice).unwrap().replace(
        "- absorbed 2.0.0, Apache-2.0 (`crates/aterm-absorbed/`)\n",
        "",
    );
    std::fs::write(&notice, text).unwrap();
    let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(!ok && log.contains("FAIL [OB-1]"), "{log}");
}

#[test]
fn a_renamed_absorbed_member_owes_the_license_obligations() {
    let fixture = absorbed_fixture("member-green", true);
    let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(
        ok,
        "a complete renamed absorbed member must attest clean:\n{log}"
    );
    assert!(
        log.contains("renamed ABSORBED member") && log.contains("[OB-7] `crates/aterm-absorbed`"),
        "the member must be attested, not skipped:\n{log}"
    );

    let fixture = absorbed_fixture("member-no-notice", true);
    std::fs::write(
        fixture.0.path().join("crates/aterm-absorbed/src/lib.rs"),
        "pub fn upstream() { /* edited */ }\n",
    )
    .unwrap();
    let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(!ok && log.contains("FAIL [OB-7]"), "{log}");

    let fixture = absorbed_fixture("member-no-license", true);
    std::fs::remove_file(fixture.0.path().join("crates/aterm-absorbed/LICENSE")).unwrap();
    let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(!ok && log.contains("FAIL [OB-5]"), "{log}");

    let fixture = absorbed_fixture("member-gone", true);
    std::fs::remove_dir_all(fixture.0.path().join("crates/aterm-absorbed")).unwrap();
    let Attestation { ok, log, .. } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(!ok && log.contains("FAIL [OB-1]"), "{log}");
}

/// An added licence text stays as its holder wrote it, so NOTICE naming it by path is its
/// notice. Unnamed, it fails; and the naming discharges neither an added file of another
/// kind nor an edited upstream licence.
#[test]
fn an_added_licence_text_takes_its_notice_from_notice() {
    let plant = |tag: &str, rel: &str, notice: &str| {
        let fixture = absorbed_fixture(tag, true);
        let root = fixture.0.path();
        std::fs::write(root.join(rel), "MIT License\n\nCopyright (c) 2022 Other\n").unwrap();
        let path = root.join("NOTICE");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(notice);
        std::fs::write(path, text).unwrap();
        report_served(root, absorbed_upstream())
    };
    let licence = "crates/aterm-absorbed/LICENSE-MIT-other";
    let named = |rel: &str| format!("Its shadow is other's, MIT: see `{rel}`.\n");

    let Attestation { ok, log, .. } = plant("licence-named", licence, &named(licence));
    assert!(ok, "a licence text NOTICE names needs no header:\n{log}");

    let Attestation { ok, log, .. } = plant("licence-unnamed", licence, "");
    assert!(
        !ok && log.contains(&format!("FAIL [OB-7] `{licence}` is a licence text added")),
        "{log}"
    );

    let data = "crates/aterm-absorbed/other.tsv";
    let Attestation { ok, log, .. } = plant("licence-not-data", data, &named(data));
    assert!(
        !ok && log.contains(&format!("FAIL [OB-7] `{data}` is added")),
        "{log}"
    );

    let upstream = "crates/aterm-absorbed/LICENSE";
    let Attestation { ok, log, .. } = plant("licence-edited", upstream, &named(upstream));
    assert!(
        !ok && log.contains(&format!("FAIL [OB-7] `{upstream}` is modified")),
        "{log}"
    );
}

/// Attest fetches the copy it diffs. Offline, or handed a download the index does not vouch
/// for, it has none: the member is UNVERIFIED, unticked and not refused, and the NOTE says
/// why.
#[test]
fn a_member_without_a_pristine_copy_is_unverified_never_ticked() {
    let fixture = absorbed_fixture("member-pristine", true);
    let Attestation {
        ok,
        log,
        unverified,
        ..
    } = report_served(fixture.0.path(), absorbed_upstream());
    assert!(ok && unverified.is_empty(), "{log}");
    assert!(
        log.contains("✓ [OB-7]")
            && log.contains("`absorbed-2.0.0.crate`, fetched from crates.io by this run"),
        "a diffed member earns the tick:\n{log}"
    );

    let mut wrong = absorbed_upstream();
    wrong.sha256 = "ab".repeat(32);
    for (case, served, why) in [
        (
            "offline",
            Served::offline(),
            "GET https://index.crates.io/ab/so/absorbed",
        ),
        (
            "wrong sha",
            wrong,
            "the download is refused and nothing was unpacked",
        ),
    ] {
        let fixture = absorbed_fixture(&format!("member-{}", case.replace(' ', "-")), true);
        std::fs::write(
            fixture.0.path().join("crates/aterm-absorbed/src/lib.rs"),
            "pub fn upstream() { /* edited */ }\n",
        )
        .unwrap();
        let Attestation {
            ok,
            log,
            unverified,
            ..
        } = report_served(fixture.0.path(), served);
        assert_eq!(unverified, ["absorbed"], "{case}: {log}");
        assert!(
            !log.contains("✓ [OB-7]")
                && log.contains("UNVERIFIED [OB-7] no file of absorbed")
                && log.contains(why),
            "{case}: an undiffed member must not be ticked, and the NOTE says why:\n{log}"
        );
        assert!(
            ok && log.contains("cargo forge attest: PASS, [OB-7] UNVERIFIED"),
            "{case}: the verdict names what was not judged, and finds nothing wrong:\n{log}"
        );
    }
}

#[test]
fn an_unreviewed_vendor_directory_still_fails_the_astream_submodule_check() {
    let fixture = good_fixture("unreviewed-bundle");
    let root = fixture.0.path();
    std::fs::create_dir_all(root.join("vendor/another-bus/src")).unwrap();
    let Attestation { ok, log, .. } = report(root);
    assert!(!ok && log.contains("vendor/another-bus"), "{log}");
    std::fs::remove_dir_all(root.join("vendor/another-bus")).unwrap();
    git(root, &["init", "-q"]);
    std::fs::create_dir_all(root.join("vendor/astream/src")).unwrap();
    let Attestation { ok, log, .. } = report(root);
    assert!(!ok && log.contains("FAIL [OB-1]"), "{log}");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = command(aterm_uds::os_path!("git"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "user.name=aterm-forge test",
            "-c",
            "user.email=forge@invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output_owned()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_ignore_sweep_does_not_descend_into_a_submodule_working_tree() {
    let fixture = good_fixture("ob10-submodule");
    let root = fixture.0.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join(".gitignore"), "target\n").unwrap();
    let sub = root.join("vendor/sub");
    std::fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q"]);
    std::fs::write(
        sub.join("lib.rs"),
        "// SPDX-License-Identifier: Apache-2.0\n",
    )
    .unwrap();
    git(&sub, &["add", "lib.rs"]);
    git(&sub, &["commit", "-q", "-m", "one"]);
    let head = git(&sub, &["rev-parse", "HEAD"]);
    git(
        root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},vendor/sub"),
        ],
    );
    assert_eq!(
        vendor_gitlinks(root),
        Some(BTreeSet::from(["vendor/sub".to_string()]))
    );
    std::fs::create_dir_all(sub.join("target/debug")).unwrap();
    std::fs::write(sub.join("target/debug/out"), "").unwrap();
    let mut log = String::new();
    assert_eq!(ob10_gitignore(root, &[], &mut log), 0, "{log}");
    std::fs::create_dir_all(root.join("vendor/goodfork/target")).unwrap();
    std::fs::write(root.join("vendor/goodfork/target/out"), "").unwrap();
    let mut log = String::new();
    assert_eq!(ob10_gitignore(root, &[], &mut log), 0, "{log}");
    // An ignored generated cache is not source. Force-adding the same path changes
    // that fact: the guard must still reject tracked source swallowed on re-vendor.
    git(root, &["add", "-f", "vendor/goodfork/target/out"]);
    let mut log = String::new();
    assert_eq!(ob10_gitignore(root, &[], &mut log), 1, "{log}");
    std::fs::write(
        root.join(".gitignore"),
        "target\n!vendor/goodfork/target/\n",
    )
    .unwrap();
    let mut log = String::new();
    assert_eq!(ob10_gitignore(root, &[], &mut log), 0, "{log}");
}

#[test]
fn required_metadata_is_checked_before_it_enters_the_index() {
    let fixture = good_fixture("ob10-required-metadata");
    let root = fixture.0.path();
    git(root, &["init", "-q"]);
    std::fs::write(
        root.join(".gitignore"),
        "Cargo.toml.orig\n.cargo_vcs_info.json\n",
    )
    .unwrap();
    let forks = survey_forks(root, &patch_paths(root).unwrap());
    assert_eq!(forks.len(), 1);
    let mut log = String::new();
    assert_eq!(ob10_gitignore(root, &forks, &mut log), 2, "{log}");
    for name in ["Cargo.toml.orig", ".cargo_vcs_info.json"] {
        std::fs::remove_file(root.join("vendor/goodfork").join(name)).unwrap();
    }
    let mut log = String::new();
    assert_eq!(ob10_gitignore(root, &forks, &mut log), 0, "{log}");
    assert!(log.contains("Cargo.toml.orig") && log.contains(".cargo_vcs_info.json"));
    assert!(ob4_provenance_files(&forks, &mut log) > 0, "{log}");
}

#[test]
fn an_apache_only_fork_needs_header_notices() {
    let fixture = good_fixture("apache");
    let root = fixture.0.path();
    std::fs::write(
        root.join("vendor/goodfork/Cargo.toml"),
        "[package]\nname = \"goodfork\"\nversion = \"1.2.3\"\nlicense = \"Apache-2.0\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("NOTICE"),
        "- goodfork 1.2.3, Apache-2.0 (`vendor/goodfork/`)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("deny.toml"),
        "[licenses]\nallow = [\"MIT\", \"Apache-2.0\"]\n",
    )
    .unwrap();
    let mut upstream: Vec<(&str, Vec<u8>)> = [
        "Cargo.toml",
        "Cargo.toml.orig",
        ".cargo_vcs_info.json",
        "LICENSE-MIT",
    ]
    .into_iter()
    .map(|rel| {
        (
            rel,
            std::fs::read(root.join("vendor/goodfork").join(rel)).unwrap(),
        )
    })
    .collect();
    upstream.push(("src/lib.rs", b"// upstream\n".to_vec()));
    let files: Vec<(&str, &[u8])> = upstream.iter().map(|(r, b)| (*r, b.as_slice())).collect();
    let served = || Served::honest("goodfork", "1.2.3", crate_file("goodfork-1.2.3", &files));
    let Attestation { ok, log, .. } = report_served(root, served());
    assert!(!ok && log.contains("FAIL [OB-7]"), "{log}");

    std::fs::write(
        root.join("vendor/goodfork/src/lib.rs"),
        "// Modified by aterm: bounds proof discharged here.\n// aterm-trust: ok\n",
    )
    .unwrap();
    let Attestation { ok, log, .. } = report_served(root, served());
    assert!(ok, "a header notice must discharge §4(b):\n{log}");

    std::fs::write(
        root.join("vendor/goodfork/src/non_utf8.bin"),
        b"unmarked binary change \xff\xfe\n",
    )
    .unwrap();
    let Attestation { ok, log, .. } = report_served(root, served());
    assert!(
        !ok && log.contains("FAIL [OB-7]"),
        "non-UTF-8 added file without notice must fail OB-7:\n{log}"
    );
    std::fs::write(
        root.join("vendor/goodfork/src/non_utf8.bin"),
        b"// Added by aterm: binary test fixture.\n\xff\xfe\n",
    )
    .unwrap();
    let Attestation { ok, log, .. } = report_served(root, served());
    assert!(
        ok,
        "non-UTF-8 added file with header notice must pass OB-7:\n{log}"
    );
}
