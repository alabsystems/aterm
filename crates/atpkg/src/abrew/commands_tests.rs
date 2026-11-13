// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn info_json_qualified_tap_cannot_be_satisfied_by_core_cache() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    prefix.ensure_api_cache().unwrap();
    std::fs::write(
        prefix.cache().join("api/formula-demo.json"),
        br#"{"name":"demo","full_name":"demo","versions":{"stable":"1"}}"#,
    )
    .unwrap();
    let tap = prefix
        .taps_dir()
        .join("hostilereviewfixture")
        .join("homebrew-other");
    std::fs::create_dir_all(tap.join("Formula")).unwrap();
    std::fs::write(
        tap.join("Formula/demo.rb"),
        "class Demo < Formula\n  version \"2\"\nend\n",
    )
    .unwrap();
    let api = Api::new(prefix.clone());
    let flags = vec!["--json=v1".into()];
    assert!(info_json(&prefix, &api, &flags, &["demo".into()]).is_ok());
    let result = info_json(
        &prefix,
        &api,
        &flags,
        &["hostilereviewfixture/other/demo".into()],
    );
    assert!(
        result
            .as_ref()
            .is_err_and(|why| why.contains("untrusted tap")),
        "qualified untrusted tap was replaced by core: {result:?}"
    );
    let flags = vec!["--json=v2".into()];
    let result = info_json(
        &prefix,
        &api,
        &flags,
        &["demo".into(), "hostilereviewfixture/missing/demo".into()],
    );
    assert!(
        result.is_err(),
        "core masked a missing qualified package: {result:?}"
    );
    std::fs::write(
        prefix.cache().join("api/cask-demo.json"),
        br#"{"token":"demo","name":["Demo"],"version":"1","url":"https://example.invalid/demo.dmg","sha256":"no_check","artifacts":[{"app":["Demo.app"]}]}"#,
    )
    .unwrap();
    std::fs::create_dir_all(tap.join("Casks")).unwrap();
    std::fs::write(
        tap.join("Casks/demo.rb"),
        "cask \"demo\" do\n  version \"2\"\nend\n",
    )
    .unwrap();
    let flags = vec!["--json=v2".into(), "--cask".into()];
    assert!(info_json(&prefix, &api, &flags, &["demo".into()]).is_ok());
    let result = info_json(
        &prefix,
        &api,
        &flags,
        &["hostilereviewfixture/other/demo".into()],
    );
    assert!(
        result
            .as_ref()
            .is_err_and(|why| why.contains("untrusted tap")),
        "qualified cask was replaced by core: {result:?}"
    );
}

#[test]
#[cfg(unix)]
fn bundle_shell_does_not_overwrite_a_preexisting_prompt_symlink() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = super::super::prefix::Prefix::at(&temp.path().join("prefix"));
    let victim = temp.path().join("unrelated-file");
    std::fs::write(&victim, "keep this").unwrap();
    let predictable = temp.path().join(format!(
        "brew-bash-prompt-{}.bash",
        super::bundle_effective_uid()
    ));
    std::os::unix::fs::symlink(&victim, &predictable).unwrap();
    let env = std::collections::BTreeMap::from([
        ("SHELL".to_string(), "/bin/bash".to_string()),
        (
            "HOMEBREW_TEMP".to_string(),
            temp.path().display().to_string(),
        ),
    ]);
    let prepared = super::bundle_shell_command(&prefix, env.clone()).unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep this");
    let second = super::bundle_shell_command(&prefix, env).unwrap();
    assert_ne!(prepared.scratch.path(), second.scratch.path());
    let startup = std::path::PathBuf::from(&prepared.command[2]);
    assert!(
        startup.is_file(),
        "startup lives until the child has exited"
    );
    drop(prepared);
    assert!(!startup.exists(), "private startup files are reclaimed");
    assert!(std::path::Path::new(&second.command[2]).is_file());
}

#[cfg(unix)]
use std::path::Path;

use super::*;

#[cfg(unix)]
#[test]
fn env_sync_links_ranges_and_never_clobbers_dirs() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let keg = tmp.path().join("Cellar/node/26.2.1");
    std::fs::create_dir_all(&keg).unwrap();
    let versions = tmp.path().join("versions");
    std::fs::create_dir_all(versions.join("26.1.0")).unwrap();
    env_sync_link("nodenv", &keg, &versions, false).unwrap();
    for v in ["26.0.0", "26.0.1", "26.1.1", "26.2.0", "26.2.1"] {
        assert_eq!(std::fs::read_link(versions.join(v)).unwrap(), keg, "{v}");
    }
    assert!(versions.join("26.1.0").is_dir());
    assert!(!versions.join("26.1.0").is_symlink());
    // Strict mode links only the exact version.
    let strict = tmp.path().join("strict-versions");
    env_sync_link("rbenv", &keg, &strict, true).unwrap();
    assert!(strict.join("26.2.1").is_symlink());
    assert!(!strict.join("26.2.0").exists());
}

#[test]
fn gist_logs_presence_is_recursive() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("logs/widget");
    assert!(!has_log_files(&root));
    std::fs::create_dir_all(root.join("sub")).unwrap();
    assert!(!has_log_files(&root));
    std::fs::write(root.join("sub/00.config.out"), b"x").unwrap();
    assert!(has_log_files(&root));
}

#[test]
fn foreign_qualified_resolution_and_errors() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let api = Api::read_only(p.clone());
    assert!(foreign_qualified("foo/bar"));
    assert!(foreign_qualified("a/b/c"));
    assert!(!foreign_qualified("homebrew/core/jq"));
    assert!(!foreign_qualified("jq"));
    // Untapped three-segment: tap-trust error.
    let text = foreign_unavailable(&p, &api, "a/b/c", false, false);
    assert!(
        text.starts_with("No available formula or cask with the name \"a/b/c\".\nThis command requires the tap a/b."),
        "{text}"
    );
    let text = foreign_unavailable(&p, &api, "a/b/c", false, true);
    assert!(
        text.starts_with("No available formula with the name \"a/b/c\"."),
        "{text}"
    );
    assert_eq!(
        foreign_unavailable(&p, &api, "foo/bar", true, false),
        "Cask 'foo/bar' is unavailable: Invalid cask name."
    );
    // Tapped three-segment: plain unavailable, no trust block.
    let tap_dir = p.taps_dir().join("a/homebrew-b");
    std::fs::create_dir_all(tap_dir.join("Formula")).unwrap();
    std::fs::create_dir_all(tap_dir.join(".git")).unwrap();
    let text = foreign_unavailable(&p, &api, "a/b/c", false, false);
    assert_eq!(
        text,
        "No available formula or cask with the name \"a/b/c\"."
    );
    // Resolvable once the tap formula file exists.
    std::fs::write(
        tap_dir.join("Formula/c.rb"),
        "class C < Formula\n  desc \"cee\"\n  homepage \"https://example.com\"\n  url \"https://example.com/c-1.0.tar.gz\"\n  sha256 \"00\"\nend\n",
    )
    .unwrap();
    assert!(foreign_formula(&p, "a/b/c").is_some());
    assert!(foreign_formula(&p, "a/c").is_none());
}

#[cfg(unix)]
#[test]
fn external_commands_glob_tap_cmd_dirs() {
    use std::os::unix::fs::PermissionsExt;
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&temp.path().join("prefix"));
    let make = |tap: &str, file: &str, mode: u32| {
        let dir = prefix.taps_dir().join(tap).join("cmd");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(file);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    make("homebrew/homebrew-core", "aspell-dictionaries.rb", 0o755);
    make("homebrew/homebrew-core", "brew-xtra", 0o755);
    make("homebrew/homebrew-core", "not-exec.rb", 0o644);
    make("someone/homebrew-thing", "sneaky.rb", 0o755);
    assert_eq!(
        external_commands(&prefix),
        vec!["aspell-dictionaries".to_string(), "xtra".to_string()]
    );
}

#[test]
fn brew_shellenv_zsh_carries_fpath_block() {
    let zsh = brew_shellenv("/opt/homebrew", "zsh");
    assert!(zsh.contains(
        "export HOMEBREW_REPOSITORY=\"/opt/homebrew\";\n\
         fpath[1,0]=\"/opt/homebrew/share/zsh/site-functions\";\n\
         export FPATH;\n\
         export PATH="
    ));
    assert!(brew_shellenv("/opt/homebrew", "-zsh").contains("fpath[1,0]="));
    assert!(!brew_shellenv("/opt/homebrew", "/bin/zsh").contains("fpath"));
    assert!(brew_shellenv("/opt/homebrew", "-fish").starts_with("set --global"));
    assert!(!brew_shellenv("/opt/homebrew", "bash").contains("fpath"));
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn unpack_destdir_expands_like_ruby_expand_path() {
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(expand_destdir("rel"), cwd.join("rel"));
    assert_eq!(expand_destdir("a/./b/../c"), cwd.join("a/c"));
    assert_eq!(expand_destdir("/abs/../x"), std::path::PathBuf::from("/x"));
    let home = std::env::var_os("HOME").unwrap();
    assert_eq!(expand_destdir("~"), std::path::PathBuf::from(&home));
    assert_eq!(
        expand_destdir("~/src"),
        std::path::PathBuf::from(&home).join("src")
    );
    assert_eq!(
        unpack_cache_dest(
            &Prefix::at(std::path::Path::new("/p")),
            "https://x/y-1.tar.gz?z"
        )
        .file_name()
        .unwrap()
        .to_string_lossy(),
        format!(
            "{}--y-1.tar.gz",
            fetch::sha256_bytes(b"https://x/y-1.tar.gz?z")
        )
    );
}

/// `bundle add` appends to what a linked Brewfile holds, and what it cannot read it leaves
/// alone: writing there follows the link, so an empty read would erase the person's file.
#[cfg(unix)]
#[test]
fn bundle_add_never_writes_over_a_brewfile_it_could_not_read() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let api = Api::new(Prefix::at(tmp.path()));
    let real = tmp.path().join("dotfiles-Brewfile");
    std::fs::write(&real, "brew \"curl\"\n").unwrap();
    let link = tmp.path().join("Brewfile");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    // A tap entry asks the formula API nothing, so this reaches the Brewfile with no network.
    let add = |name: &str| {
        bundle_add_entries(
            &api,
            &[
                format!("--file={}", link.display()),
                "--tap".to_string(),
                name.to_string(),
            ],
        )
    };
    let code = add("acme/one");
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "brew \"curl\"\ntap \"acme/one\"\n"
    );

    let oversized = vec![b'#'; 4 * 1024 * 1024 + 1];
    std::fs::write(&real, &oversized).unwrap();
    let code = add("acme/two");
    assert_ne!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    assert_eq!(std::fs::read(&real).unwrap(), oversized);
}

fn dependent_order(
    graph: &[(&str, &[&str])],
    selected: &[&str],
) -> Result<Vec<(Formula, String)>, String> {
    let formulas: std::collections::BTreeMap<_, _> = graph
        .iter()
        .map(|(name, dependencies)| {
            (
                name.to_string(),
                Formula {
                    name: name.to_string(),
                    dependencies: dependencies.iter().map(|name| name.to_string()).collect(),
                    ..Formula::default()
                },
            )
        })
        .collect();
    let rows = selected
        .iter()
        .map(|name| (formulas[*name].clone(), format!("installed-{name}")))
        .collect();
    order_dependent_upgrades(rows, |name| formulas.get(name).cloned())
}

#[test]
fn dependent_upgrade_order_uses_a_ready_frontier_instead_of_pairwise_comparison() {
    // Pairwise dependency-first/name-second says a < b < c but a > c on this
    // ordinary DAG. Lexical input leaves a dependency after its dependent.
    let graph: &[(&str, &[&str])] = &[("a", &["c"]), ("b", &["z"]), ("c", &["z"]), ("z", &[])];
    for selected in [
        ["a", "b", "c"],
        ["a", "c", "b"],
        ["b", "a", "c"],
        ["b", "c", "a"],
        ["c", "a", "b"],
        ["c", "b", "a"],
    ] {
        let ordered = dependent_order(graph, &selected).expect("an acyclic upgrade plan");
        assert_eq!(
            ordered
                .iter()
                .map(|(f, _)| f.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "c", "a"],
            "the first available name is deterministic for {selected:?}"
        );
        for (formula, installed) in ordered {
            assert_eq!(installed, format!("installed-{}", formula.name));
        }
    }
}

#[test]
fn dependent_upgrade_order_keeps_transitive_edges_through_unselected_formulae() {
    let graph: &[(&str, &[&str])] = &[
        ("app", &["homebrew/core/bridge"]),
        ("bridge", &["middle"]),
        ("middle", &["leaf", "leaf"]),
        ("leaf", &[]),
    ];
    let ordered = dependent_order(graph, &["app", "leaf", "middle"]).unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|(f, _)| f.name.as_str())
            .collect::<Vec<_>>(),
        ["leaf", "middle", "app"],
        "unchanged bridge metadata constrains ordering without adding an upgrade"
    );
    let aliases = dependent_order(graph, &["leaf", "leaf"]).unwrap();
    assert_eq!(aliases.len(), 2, "every selected row survives ordering");
}

#[test]
fn cyclic_dependent_upgrades_refuse_the_whole_plan() {
    let graph: &[(&str, &[&str])] = &[
        ("a", &["bridge"]),
        ("bridge", &["b"]),
        ("b", &["a"]),
        ("free", &[]),
        ("self", &["self"]),
    ];
    for selected in [vec!["free", "a", "b"], vec!["b", "a"], vec!["self"]] {
        assert!(
            dependent_order(graph, &selected).is_err(),
            "no partial plan escapes a runtime cycle: {selected:?}"
        );
    }
    assert!(dependent_order(graph, &[]).unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn unlinked_keg_judges_only_receipt_recorded_linkable_kegs() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let mk = |name: &str, keg_only: Option<&str>, with_bin: bool| {
        let dir = p.keg(name, "1.0");
        std::fs::create_dir_all(&dir).unwrap();
        let mut fields = vec![
            ("installed_by", "abrew"),
            ("name", name),
            ("version", "1.0"),
        ];
        if let Some(v) = keg_only {
            fields.push(("keg_only", v));
        }
        p.write_receipt(&dir, &fields).unwrap();
        if with_bin {
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(dir.join("bin").join(name), b"#!/bin/sh\n").unwrap();
        }
        super::super::prefix::Installed {
            name: name.to_string(),
            version: "1.0".to_string(),
            kind: Kind::Formula,
            versions: vec!["1.0".to_string()],
        }
    };
    let demo = mk("demo", Some("false"), true);
    assert!(unlinked_keg(&p, &demo));
    assert_eq!(keg_bin_linked(&p, "demo", "1.0"), Some(false));
    assert_eq!(linked_keg_version(&p, &demo), None);
    let linked = p.bin().join("demo");
    std::fs::write(&linked, b"foreign file").unwrap();
    assert!(unlinked_keg(&p, &demo));
    assert_eq!(linked_keg_version(&p, &demo), None);
    std::fs::remove_file(&linked).unwrap();
    let foreign = p.keg("other", "1.0").join("bin/demo");
    std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    std::fs::write(&foreign, b"other keg").unwrap();
    super::super::prefix::symlink_file(&foreign, &linked).unwrap();
    assert!(unlinked_keg(&p, &demo));
    assert_eq!(linked_keg_version(&p, &demo), None);
    std::fs::remove_file(&linked).unwrap();
    let newer = p.keg("demo", "2.0");
    std::fs::create_dir_all(newer.join("bin")).unwrap();
    std::fs::write(newer.join("bin/demo"), b"newer version").unwrap();
    p.write_receipt(
        &newer,
        &[
            ("installed_by", "abrew"),
            ("name", "demo"),
            ("version", "2.0"),
            ("keg_only", "false"),
        ],
    )
    .unwrap();
    let admitted = p.installed_one(Kind::Formula, "demo").unwrap();
    super::super::prefix::symlink_file(&p.keg("demo", "1.0"), &p.opt("demo")).unwrap();
    assert_eq!(p.optlinked_version("demo").as_deref(), Some("1.0"));
    super::super::prefix::symlink_file(Path::new("../Cellar/demo/2.0/bin/demo"), &linked).unwrap();
    assert!(unlinked_keg(&p, &demo));
    assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("2.0"));
    std::fs::remove_file(&linked).unwrap();
    super::super::prefix::symlink_file(Path::new("../Cellar/demo/1.0/bin/demo"), &linked).unwrap();
    assert!(!unlinked_keg(&p, &demo));
    assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("1.0"));
    std::fs::write(newer.join("bin/demo-newer"), b"second command").unwrap();
    let second = p.bin().join("demo-newer");
    super::super::prefix::symlink_file(Path::new("../Cellar/demo/2.0/bin/demo-newer"), &second)
        .unwrap();
    assert_eq!(linked_keg_version(&p, &admitted), None);
    std::fs::remove_file(second).unwrap();
    assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("1.0"));
    std::fs::remove_file(&linked).unwrap();
    super::super::prefix::symlink_file(
        &p.keg("demo", "1.0").join("bin/demo"),
        &p.bin().join("demo"),
    )
    .unwrap();
    assert!(!unlinked_keg(&p, &demo));
    for item in [
        mk("kegonly", Some("true"), true),
        mk("legacy", None, true),
        mk("libonly", Some("false"), false),
    ] {
        assert!(!unlinked_keg(&p, &item));
        assert_eq!(keg_bin_linked(&p, &item.name, &item.version), None);
        assert_eq!(linked_keg_version(&p, &item), None);
    }
    let empty = mk("emptybin", Some("false"), false);
    std::fs::create_dir(p.keg("emptybin", "1.0").join("bin")).unwrap();
    assert_eq!(keg_bin_linked(&p, &empty.name, &empty.version), None);
    assert_eq!(linked_keg_version(&p, &empty), None);
    let unreadable = mk("unreadable", Some("false"), false);
    std::fs::write(p.keg("unreadable", "1.0").join("bin"), b"not a directory").unwrap();
    assert_eq!(
        keg_bin_linked(&p, &unreadable.name, &unreadable.version),
        None
    );
    assert!(!unlinked_keg(&p, &unreadable));
    assert_eq!(linked_keg_version(&p, &unreadable), None);
}

#[test]
fn prune_downloads_follows_brew_stale_rules() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure_api_cache().unwrap();
    let api = Api::new(p.clone());
    let write_doc = |p: &Prefix| {
        std::fs::write(
            p.cache().join("api/formula-demo.json"),
            br#"{"name":"demo","versions":{"stable":"2"}}"#,
        )
        .unwrap();
    };
    write_doc(&p);
    owned_keg(&p, "demo", "2");
    let downloads = p.cache().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let files = [
        "demo--1.all.bottle.tar.gz",
        "demo--2.all.bottle.tar.gz",
        "demo-1.bottle_manifest.json",
        "demo-2.bottle_manifest.json",
        "other--1.all.bottle.tar.gz",
        "stray.bin",
        "demo--1.all.bottle.tar.gz.incomplete",
    ];
    for f in files {
        std::fs::write(downloads.join(f), b"x").unwrap();
    }
    let default_mode = DownloadCleanup {
        days: std::time::Duration::from_secs(120 * 86_400),
        explicit_prune: false,
        scrub: false,
    };
    // Default: the old-version bottle + manifest and the
    // incomplete download go; current, uninstalled, and
    // unrecognized files stay.
    let (removed, _) =
        prune_downloads(&p, &api, true, &default_mode, &[], &mut Vec::new()).unwrap();
    assert_eq!(removed, 3);
    for f in files {
        assert!(downloads.join(f).exists(), "{f}");
    }
    // Named scope only considers that formula's files.
    let (removed, _) =
        prune_downloads(&p, &api, true, &default_mode, &["other"], &mut Vec::new()).unwrap();
    assert_eq!(removed, 0);
    // An explicit --prune disables the stale check: with
    // a huge age nothing goes; with age zero everything
    // (incomplete included) goes.
    let huge = DownloadCleanup {
        days: std::time::Duration::from_secs(999_999_999),
        explicit_prune: true,
        scrub: false,
    };
    let (removed, _) = prune_downloads(&p, &api, true, &huge, &[], &mut Vec::new()).unwrap();
    assert_eq!(removed, 1, "only the incomplete download");
    let zero = DownloadCleanup {
        days: std::time::Duration::ZERO,
        explicit_prune: true,
        scrub: false,
    };
    let (removed, _) = prune_downloads(&p, &api, false, &zero, &[], &mut Vec::new()).unwrap();
    // --prune=all takes the API documents too (brew's
    // cache_files entries all pass prune? at zero days).
    assert_eq!(removed, files.len() + 1);
    assert!(!p.cache().join("api/formula-demo.json").exists());
    // Scrub also removes the API documents.
    write_doc(&p);
    let scrub = DownloadCleanup {
        days: std::time::Duration::from_secs(120 * 86_400),
        explicit_prune: false,
        scrub: true,
    };
    let (removed, _) = prune_downloads(&p, &api, false, &scrub, &[], &mut Vec::new()).unwrap();
    assert_eq!(removed, 1);
    assert!(!p.cache().join("api/formula-demo.json").exists());
}

#[test]
fn edit_distance_measures_typo_closeness() {
    assert_eq!(edit_distance("hello", "hello"), 0);
    assert_eq!(edit_distance("helo", "hello"), 1);
    assert_eq!(edit_distance("wget", "wgwt"), 1);
    assert_eq!(edit_distance("wget", "wgt"), 1);
    assert_eq!(edit_distance("tree", "tpe"), 2);
    assert!(edit_distance("hello", "goodbye") > 2);
    assert_eq!(edit_distance("", "ab"), 2);
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn tap_json_builders_mirror_brew_shape() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let tap = p.taps_dir().join("searchtest").join("homebrew-probe");
    std::fs::create_dir_all(tap.join("Formula")).expect("mkdir");
    std::fs::create_dir_all(tap.join("Casks")).expect("mkdir");
    std::fs::write(
        tap.join("Formula/custool.rb"),
        "class Custool < Formula\n  desc \"Probe tool\"\n  url \"https://example.com/x.tgz\"\n  version \"1.0\"\n  depends_on \"cmake\" => :build\n  depends_on \"zlib\"\nend\n",
    )
    .expect("write");
    std::fs::write(
        tap.join("Casks/custapp.rb"),
        "cask \"custapp\" do\n  version \"1.0\"\n  url \"https://example.com/c.dmg\"\n  name \"CustApp\"\n  app \"CustApp.app\"\nend\n",
    )
    .expect("write");
    let f = tap::lookup(&p, "custool").expect("formula");
    assert_eq!(f.build_dependencies, ["cmake"]);
    assert_eq!(f.dependencies, ["zlib"]);
    let (value, order) = tap_formula_json(&p, &f).expect("formula json");
    let text = jpretty(&JVal::Ordered(Box::new(value), order));
    let name_at = text.find("\"name\": \"custool\"").expect("name");
    let license_at = text.find("\"license\": null").expect("license");
    let checksum_at = text.find("\"ruby_source_checksum\"").expect("checksum");
    assert!(name_at < license_at && license_at < checksum_at, "{text}");
    assert!(
        text.contains("\"build_dependencies\": [\n    \"cmake\""),
        "{text}"
    );
    assert!(
        text.contains("\"ruby_source_path\": \"Formula/custool.rb\""),
        "{text}"
    );
    let (c, qualified) = tap::lookup_cask(&p, "custapp").expect("cask");
    let (value, order) = tap_cask_json(&p, &c, &qualified).expect("cask json");
    let text = jpretty(&JVal::Ordered(Box::new(value), order));
    assert!(
        text.contains("\"full_token\": \"searchtest/probe/custapp\""),
        "{text}"
    );
    assert!(
        text.contains("\"target\": \"/Applications/CustApp.app\""),
        "{text}"
    );
    assert!(
        text.contains("\"ruby_source_path\": \"Casks/custapp.rb\""),
        "{text}"
    );
}

#[test]
fn tap_cask_records_parse_and_resolve() {
    let text = r#"cask "custapp" do
  version "1.2.3"
  sha256 arm:   "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
         intel: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

  url "https://example.com/custapp-#{version.major_minor}.dmg"
  name "CustApp"
  desc "Probe app"
  homepage "https://example.com/"

  auto_updates true
  depends_on macos: ">= :sonoma"
  depends_on cask: "helper"

  app "CustApp.app"
  binary "custapp-cli", target: "custapp"

  zap trash: [
    "~/Library/CustApp",
    "~/Library/Preferences/com.example.custapp.plist",
  ]
end
"#;
    let c = tap::parse_cask_rb_as_data(text, "custapp");
    assert_eq!(c.version, "1.2.3");
    assert_eq!(
        c.sha256.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(c.url, "https://example.com/custapp-1.2.dmg");
    assert_eq!(c.name, "CustApp");
    assert!(c.auto_updates);
    assert_eq!(c.macos_min.as_deref(), Some("14"));
    assert_eq!(c.depends_cask, ["helper"]);
    assert_eq!(c.artifacts.len(), 2);
    assert_eq!(c.zap_trash.len(), 2);
    assert!(c.zap_present);
    assert!(c.unsupported_constraints.is_none());
    let nocheck = tap::parse_cask_rb_as_data(
        "cask \"x\" do\n  version :latest\n  sha256 :no_check\n  url \"https://e.com/x.dmg\"\n  app \"X.app\"\nend\n",
        "x",
    );
    assert_eq!(nocheck.version, "latest");
    assert!(nocheck.sha256.is_none() && nocheck.sha256_no_check);
}

#[test]
fn tap_lookup_and_factory_gate_cover_plain_tap_directories() {
    // Brew resolves names from any tap directory (no .git
    // checkout required) and factory-refuses untrusted
    // ones. The fixture tap is untrusted in any default
    // trust store.
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let tap = p.taps_dir().join("searchtest").join("homebrew-probe");
    std::fs::create_dir_all(tap.join("Formula")).expect("mkdir");
    std::fs::write(
        tap.join("Formula/custool.rb"),
        "class Custool < Formula\n  url \"https://example.com/x.tgz\"\n  version \"1.0\"\nend\n",
    )
    .expect("write");
    let api = Api::read_only(p.clone());
    let found = tap::lookup(&p, "custool").expect("plain tap dir resolves by leaf");
    assert_eq!(found.full_name, "searchtest/probe/custool");
    assert!(factory_formula(&p, &api, "custool").is_err());
    assert!(factory_formula(&p, &api, "searchtest/probe/custool").is_err());
}

#[test]
fn search_covers_tap_names_with_brew_trust_semantics() {
    // A tap directory with one formula + one cask, against
    // a two-entry core index. The tap is untrusted in any
    // default trust store: brew prints an exact substring
    // formula hit, dies on the fuzzy load, and never shows
    // the cask.
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let api_dir = p.cache().join("api");
    std::fs::create_dir_all(&api_dir).expect("mkdir");
    std::fs::write(
        api_dir.join("formula-index.json"),
        r#"[{"name":"cuetools","desc":"cue tools"}]"#,
    )
    .expect("write");
    std::fs::write(
        api_dir.join("cask-index.json"),
        r#"[{"token":"caffeine","desc":"keep awake"}]"#,
    )
    .expect("write");
    let tap = p.taps_dir().join("searchtest").join("homebrew-probe");
    std::fs::create_dir_all(tap.join("Formula")).expect("mkdir");
    std::fs::create_dir_all(tap.join("Casks")).expect("mkdir");
    std::fs::write(
        tap.join("Formula/custool.rb"),
        "class Custool < Formula\n  url \"https://example.com/x.tgz\"\n  version \"1.0\"\nend\n",
    )
    .expect("write");
    std::fs::write(tap.join("Casks/custapp.rb"), "cask \"custapp\" do\nend\n").expect("write");
    let api = Api::read_only(p.clone());
    let empty = std::collections::BTreeSet::new();
    let hits = |q: &str| {
        collect_name_hits(
            &p,
            &api,
            &SearchMode::Text(q.to_string()),
            q,
            true,
            true,
            &empty,
            &empty,
        )
    };
    let (formula_hits, cask_hits, _) = hits("custool").expect("exact substring prints");
    assert_eq!(formula_hits, ["searchtest/probe/custool", "cuetools"]);
    assert!(cask_hits.is_empty(), "{cask_hits:?}");
    assert!(hits("custol").is_err(), "fuzzy load of untrusted tap dies");
    let (_, cask_hits, _) = hits("custapp").expect("cask search runs");
    assert!(
        cask_hits.is_empty(),
        "untrusted tap cask hidden: {cask_hits:?}"
    );
}

#[test]
fn regex_search_matches_brew_case_and_anchoring() {
    assert_eq!(search_regex_pattern("/wget.+/"), Some("wget.+"));
    assert_eq!(search_regex_pattern("/http client/"), Some("http client"));
    assert_eq!(search_regex_pattern("//"), Some(""));
    assert_eq!(search_regex_pattern("/"), None);
    assert_eq!(search_regex_pattern("/wget"), None);
    assert_eq!(search_regex_pattern("wget/"), None);
    assert_eq!(search_regex_pattern("wget"), None);
    assert_eq!(search_regex_pattern("/a\nb/"), None);

    let query = |p: &str| SearchQuery::Regex(compile_search_regex(p).unwrap());
    assert!(query("wget").hits_any(&["wget2"]).unwrap());
    assert!(!query("WGET").hits_any(&["wget"]).unwrap());
    assert!(query("(?i)WGET").hits_any(&["wget"]).unwrap());
    assert!(query("get").hits_any(&["wget"]).unwrap());
    assert!(!query("^get").hits_any(&["wget"]).unwrap());
    assert!(query("^lib.*@\\d$").hits_any(&["libfoo@3"]).unwrap());
    assert!(query("^py").hits_any(&["py-spy"]).unwrap());
    assert!(!query("^py").hits_any(&["airspy"]).unwrap());
    assert!(query("").hits_any(&["anything"]).unwrap());
    assert!(!query("HTTP").hits_any(&["wget"]).unwrap());
    assert!(query("HTTP").hits_any(&["wget", "HTTP client"]).unwrap());

    for refused in [r"(?=http)client", r"(\w)\1", r"\p{L}+", "(a{200}){200}"] {
        assert!(compile_search_regex(refused).is_err(), "{refused}");
    }
}

#[test]
fn text_search_simplifies_like_brew_and_desc_fields_select() {
    assert_eq!(simplify_search_text("Node.js C++ @3"), "nodejsc++@3");
    let q = SearchQuery::text("node js");
    assert!(q.hits_any(&["node", "Platform built on Node.js"]).unwrap());
    assert!(SearchQuery::text("alt-tab").hits_any(&["alt-tab"]).unwrap());
    assert!(!q.hits_any(&["deno", "A runtime"]).unwrap());

    let by = |s: &str| SearchQuery::text(s);
    assert!(desc_matches(&by("wget"), DescField::Name, false, "wget", "", "HTTP").unwrap());
    assert!(!desc_matches(&by("http"), DescField::Name, false, "wget", "", "HTTP").unwrap());
    assert!(
        desc_matches(
            &by("http"),
            DescField::Description,
            false,
            "wget",
            "",
            "HTTP client"
        )
        .unwrap()
    );
    assert!(
        !desc_matches(
            &by("wget"),
            DescField::Description,
            false,
            "wget",
            "",
            "HTTP client"
        )
        .unwrap()
    );
    assert!(
        desc_matches(
            &by("firefox"),
            DescField::Description,
            true,
            "firefox",
            "Firefox",
            "Web browser"
        )
        .unwrap()
    );
    assert!(
        desc_matches(
            &by("browser"),
            DescField::Either,
            true,
            "firefox",
            "Firefox",
            "Web browser"
        )
        .unwrap()
    );
    assert!(
        !desc_matches(
            &by("safari"),
            DescField::Either,
            true,
            "firefox",
            "Firefox",
            "Web browser"
        )
        .unwrap()
    );
    let re = SearchQuery::Regex(compile_search_regex("HTTP").unwrap());
    assert!(
        desc_matches(
            &re,
            DescField::Description,
            false,
            "wget",
            "",
            "HTTP client"
        )
        .unwrap()
    );
    assert!(
        !desc_matches(
            &re,
            DescField::Description,
            false,
            "wget",
            "",
            "http client"
        )
        .unwrap()
    );
}

fn owned_keg(prefix: &Prefix, name: &str, version: &str) {
    let dir = prefix.keg(name, version);
    std::fs::create_dir_all(&dir).unwrap();
    prefix
        .write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("name", name),
                ("version", version),
            ],
        )
        .unwrap();
}

#[test]
fn outdated_rows_resolve_tap_kegs_and_skip_the_rest() {
    // Kegs only a tap answers resolve there (trusted taps
    // only — brew's unnamed outdated skips untrusted kegs
    // silently); kegs nothing answers are skipped too,
    // never an error poisoning the whole listing.
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    let tap_formula = |user: &str, repo: &str, name: &str| {
        let dir = temp
            .path()
            .join(format!("Library/Taps/{user}/homebrew-{repo}/Formula"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.rb")),
            format!(
                "class {} < Formula\n  url \"https://example.com/{name}.tgz\"\n  version \"2\"\nend\n",
                name.chars().next().unwrap().to_uppercase().collect::<String>()
                    + &name[1..]
            ),
        )
        .unwrap();
    };
    tap_formula("homebrew", "fixture", "fixtap");
    tap_formula("zzfixture999", "other", "othertool");
    owned_keg(&prefix, "fixtap", "1");
    owned_keg(&prefix, "othertool", "1");
    owned_keg(&prefix, "ghost", "1");
    let api = Api::new(prefix.clone());
    let rows = outdated_rows(&prefix, &api, false, Greedy::default()).unwrap();
    assert_eq!(
        rows,
        [("fixtap".to_string(), "1".to_string(), "2".to_string())],
        "official-tap keg resolves; untrusted zzfixture999/othertool and unresolvable ghost skip"
    );
}

#[test]
fn analytics_table_matches_brew_layout_at_80_columns() {
    assert_eq!(format_count(0), "0");
    assert_eq!(format_count(999), "999");
    assert_eq!(format_count(59_538), "59,538");
    assert_eq!(format_count(1_234_567), "1,234,567");
    let rows = [
        ("wget".to_string(), 59_416),
        ("wget --HEAD".to_string(), 122),
    ];
    let table = analytics_table("install", "30", &rows, 80);
    let expected = concat!(
        "==> install (30 days)\n",
        "Index | Name (with options)                                  |  Count |  Percent\n",
        "-----:|------------------------------------------------------|-------:|--------:\n",
        "1     | wget                                                 | 59,416 |   99.80%\n",
        "2     | wget --HEAD                                          |    122 |    0.20%\n",
        "Total |                                                      | 59,538 |  100.00%\n",
    );
    assert_eq!(table, expected);
    let one = analytics_table("install", "30", &rows[..1], 80);
    assert!(!one.contains("Total"), "{one}");
    assert!(one.contains("100.00%"), "{one}");
}

#[test]
fn deprecate_disable_message_matches_brew_rendering() {
    use super::super::formula::DeprecateDisable;
    let dd = DeprecateDisable {
        disable_reason: Some("fails_gatekeeper_check".to_string()),
        disable_date: Some("2999-01-01".to_string()),
        ..Default::default()
    };
    assert_eq!(
        deprecate_disable_message(true, false, true, &dd).as_deref(),
        Some(
            "Disabled because it does not pass the macOS Gatekeeper check! \
             It will be disabled on 2999-01-01."
        )
    );
    // Deprecation date shifts twelve months (leap day clamps)
    // to the synthesized disable date.
    let dd = DeprecateDisable {
        deprecation_reason: Some("unmaintained".to_string()),
        deprecation_date: Some("2000-02-29".to_string()),
        deprecation_replacement_formula: Some("newtool".to_string()),
        ..Default::default()
    };
    assert_eq!(
        deprecate_disable_message(false, true, false, &dd).as_deref(),
        Some(
            "Deprecated because it is not maintained upstream! \
             It was disabled on 2001-02-28.\nReplacement:\n  brew install --formula newtool\n"
        )
    );
    // A free-form reason passes through; no reason, bare kind.
    let dd = DeprecateDisable {
        disable_reason: Some("is broken in mysterious ways".to_string()),
        ..Default::default()
    };
    assert_eq!(
        deprecate_disable_message(false, false, true, &dd).as_deref(),
        Some("Disabled because it is broken in mysterious ways!")
    );
    assert_eq!(
        deprecate_disable_message(true, true, false, &DeprecateDisable::default()).as_deref(),
        Some("Deprecated!")
    );
    assert!(deprecate_disable_message(false, false, false, &dd).is_none());
}

#[test]
fn analytics_summary_totals_each_period_in_brew_category_order() {
    let doc = aterm_json::json!({
        "analytics": {
            "build_error": {"30d": {"wget": 23}},
            "install_on_request": {
                "30d": {"wget": 46000, "wget --HEAD": 187},
                "90d": {"wget": 88049},
                "365d": {"wget": 321504}
            },
            "install": {
                "30d": {"wget": 46322, "wget --HEAD": 122},
                "90d": {"wget": 88791},
                "365d": {"wget": 325445}
            }
        }
    });
    assert_eq!(
        analytics_summary_lines(Some(&doc)),
        [
            "install: 46,444 (30 days), 88,791 (90 days), 325,445 (365 days)",
            "install-on-request: 46,187 (30 days), 88,049 (90 days), 321,504 (365 days)",
            "build-error: 23 (30 days)",
        ]
    );
    assert!(analytics_summary_lines(None).is_empty());
    assert!(analytics_summary_lines(Some(&aterm_json::json!({"analytics": null}))).is_empty());
}

#[test]
fn multiple_version_lines_keep_only_multi_version_formulae_sorted() {
    let row = |name: &str, versions: &[&str]| Installed {
        name: name.into(),
        version: versions
            .last()
            .map(|v| (*v).to_string())
            .unwrap_or_default(),
        kind: Kind::Formula,
        versions: versions.iter().map(|v| (*v).to_string()).collect(),
    };
    let rows = [
        row("zeta", &["1", "2"]),
        row("solo", &["3"]),
        row("alpha", &["0.9", "1.0", "1.1"]),
        row("none", &[]),
    ];
    assert_eq!(
        multiple_version_lines(&rows),
        ["alpha 0.9 1.0 1.1", "zeta 1 2"]
    );
}

#[test]
fn flat_dependencies_close_over_the_full_graph_sorted() {
    let formula = |name: &str, deps: &[&str]| Formula {
        name: name.into(),
        dependencies: deps.iter().map(|name| (*name).to_string()).collect(),
        ..Formula::default()
    };
    let app = formula("app", &["zeta", "homebrew/core/mid", "zeta"]);
    let read = |name: &str| match name {
        "mid" => Some(formula("mid", &["leaf"])),
        "leaf" => Some(formula("leaf", &["app"])),
        "zeta" => Some(formula("zeta", &[])),
        _ => None,
    };
    let direct = flat_dependencies(&app, true, |_| {
        panic!("direct sets read nothing below the root")
    });
    assert_eq!(
        direct.iter().collect::<Vec<_>>(),
        [&"mid".to_string(), &"zeta".to_string()]
    );
    let all = flat_dependencies(&app, false, read);
    assert_eq!(
        all.iter().collect::<Vec<_>>(),
        [&"leaf".to_string(), &"mid".to_string(), &"zeta".to_string()]
    );
}

#[test]
fn completion_caveats_follow_brews_keg_checks() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    let keg = tmp.path().join("keg");
    for (dir, file) in [
        ("share/zsh/site-functions", "_demo"),
        ("share/fish/vendor_completions.d", "demo.fish"),
        ("share/fish/vendor_functions.d", "demo_fn.fish"),
        ("etc/bash_completion.d", "demo"),
        ("share/emacs/site-lisp/demo", "demo.el"),
    ] {
        let dir = keg.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), b"").unwrap();
    }
    let f = Formula {
        name: "demo".to_string(),
        ..Formula::default()
    };
    let root = prefix.root.display().to_string();
    assert_eq!(
        completions_and_elisp_for(&prefix, &f, &keg, &["zsh"], &|_| true),
        vec![
            format!("zsh completions have been installed to:\n  {root}/share/zsh/site-functions\n"),
            format!(
                "Emacs Lisp files have been installed to:\n  {root}/share/emacs/site-lisp/demo\n"
            ),
        ]
    );
    assert_eq!(
        completions_and_elisp_for(&prefix, &f, &keg, &["fish"], &|_| true)[0],
        format!(
            "fish completions and functions have been installed to:\n  {root}/share/fish/vendor_completions.d\n  {root}/share/fish/vendor_functions.d\n"
        )
    );
    // A shell not on PATH contributes nothing.
    assert_eq!(
        completions_and_elisp_for(&prefix, &f, &keg, &["zsh"], &|_| false).len(),
        1,
        "only the elisp line remains"
    );
    // Keg-only formulae point at the opt prefix and skip elisp.
    let keg_only = Formula {
        name: "demo".to_string(),
        keg_only: true,
        ..Formula::default()
    };
    assert_eq!(
        completions_and_elisp_for(&prefix, &keg_only, &keg, &["bash"], &|_| true),
        vec![format!(
            "Bash completion has been installed to:\n  {}/etc/bash_completion.d\n",
            prefix.opt("demo").display()
        )]
    );
}

#[test]
fn strip_bottle_block_matches_brew_api_source() {
    let full = "class Hello < Formula\n  license \"GPL\"\n\n  bottle do\n    rebuild 1\n    sha256 cellar: :any, arm64: \"x\"\n  end\n\n  def install\n  end\nend\n";
    assert_eq!(
        strip_bottle_block(full),
        "class Hello < Formula\n  license \"GPL\"\n\n  def install\n  end\nend\n"
    );
    // No block, and a block with no closing end, pass through.
    assert_eq!(strip_bottle_block("class X\nend\n"), "class X\nend\n");
    let unclosed = "  bottle do\n    sha256 \"x\"\n";
    assert_eq!(strip_bottle_block(unclosed), unclosed);
    // A `pour_bottle?` line is not a bottle block.
    let pour = "  def pour_bottle?\n    true\n  end\n";
    assert_eq!(strip_bottle_block(pour), pour);
}

#[test]
fn erb_url_encode_matches_ruby() {
    assert_eq!(
        erb_url_encode("a b~c@d:e#f_g-h.i"),
        "a%20b~c%40d%3Ae%23f_g-h.i"
    );
    // The GraphvizOnline fragment brew 7.0 opens for `deps --graph jq`.
    assert_eq!(
        erb_url_encode("digraph {\n  \"jq\" -> \"oniguruma\"\n}"),
        "digraph%20%7B%0A%20%20%22jq%22%20-%3E%20%22oniguruma%22%0A%7D"
    );
}

#[test]
fn deps_tree_matches_brews_continuations_and_circular_marks() {
    let row = |name: &str, deps: &[&str]| super::super::api::DepRow {
        name: name.to_string(),
        full_name: name.to_string(),
        deps: deps.iter().map(|d| (*d).to_string()).collect(),
        ..Default::default()
    };
    let rows: std::collections::BTreeMap<String, super::super::api::DepRow> = [
        row("app", &["kept", "last"]),
        row("kept", &["leaf", "child"]),
        row("leaf", &["kept"]),
        row("child", &[]),
        row("last", &["leaf"]),
    ]
    .into_iter()
    .map(|r| (r.name.clone(), r))
    .collect();
    let read = |name: &str| rows.get(name).cloned();
    let never = |_: &str| false;
    let expander = DepExpander::new(
        &read,
        super::super::api::DepFilter::default(),
        false,
        &never,
    );
    let display = |_: &DepExpander, name: &str, _: &super::super::api::DepTags| name.to_string();
    let filter = super::super::api::DepFilter::default();
    let mut lines = Vec::new();
    let mut seen = Default::default();
    let mut circular = false;
    deps_tree_lines(
        &expander,
        &rows["app"],
        &filter,
        false,
        &never,
        false,
        true,
        false,
        &Prefix::at(std::path::Path::new("/tmp/abrew-test-nonexistent")),
        &display,
        &mut seen,
        "",
        &mut circular,
        &mut lines,
    );
    assert_eq!(
        lines,
        [
            "├── kept",
            "│   ├── leaf",
            "│   │   └── kept (CIRCULAR DEPENDENCY)",
            "│   └── child",
            "└── last",
            "    └── leaf",
            "        └── kept",
            "            ├── leaf (CIRCULAR DEPENDENCY)",
            "            └── child",
        ]
    );
    assert!(circular, "circular roots fail the command like brew");

    let mut pruned = Vec::new();
    let mut seen = Default::default();
    let mut circular = false;
    deps_tree_lines(
        &expander,
        &rows["app"],
        &filter,
        false,
        &never,
        true,
        true,
        false,
        &Prefix::at(std::path::Path::new("/tmp/abrew-test-nonexistent")),
        &display,
        &mut seen,
        "",
        &mut circular,
        &mut pruned,
    );
    assert_eq!(
        pruned,
        [
            "├── kept",
            "│   ├── leaf",
            "│   │   └── kept (CIRCULAR DEPENDENCY)",
            "│   └── child",
            "└── last",
            "    └── leaf (PRUNED)",
        ]
    );
}

#[test]
#[cfg(unix)]
fn long_listing_dates_keep_each_instants_offset_and_handle_extreme_ages() {
    const NOW: i64 = 1_800_000_000;
    const OLD: i64 = NOW - 400 * 86_400;
    const BEFORE: i64 = 1_710_063_000;
    const AFTER: i64 = BEFORE + 3_600;
    assert_eq!(BEFORE.div_euclid(86_400), AFTER.div_euclid(86_400));
    let mut dated = LongLister {
        users: Default::default(),
        groups: Default::default(),
        offsets: [
            (NOW, 0),
            (OLD, 0),
            (BEFORE, -28_800),
            (AFTER, -25_200),
            (-1, 0),
            (i64::MAX, 3_600),
            (i64::MIN, -3_600),
        ]
        .into(),
        now: NOW,
    };
    assert_eq!(dated.date(NOW), "Jan 15 08:00");
    assert_eq!(dated.date(OLD), "Dec 11  2025");
    dated.now = AFTER;
    assert_eq!(dated.date(BEFORE), "Mar 10 01:30");
    assert_eq!(dated.date(AFTER), "Mar 10 03:30");

    for (now, mtime, expected) in [
        (i64::MIN, BEFORE, "Mar 10  2024"),
        (i64::MAX, -1, "Dec 31  1969"),
        (i64::MAX, i64::MAX, "Dec  4 16:30"),
        (i64::MIN, i64::MIN, "Jan 27 07:29"),
    ] {
        dated.now = now;
        assert_eq!(dated.date(mtime), expected, "now={now}, mtime={mtime}");
    }
}

#[test]
fn core_shard_puts_lib_formulae_under_lib() {
    assert_eq!(core_shard("libffi"), "lib");
    assert_eq!(core_shard("lib"), "lib");
    assert_eq!(core_shard("tree"), "t");
    assert_eq!(core_shard("openssl@3"), "o");
}

#[test]
fn macho_magic_and_executable_filetype() {
    assert!(is_macho(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01]));
    assert!(!is_macho(b"#!/bin/sh"));
    assert!(
        !is_macho(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52]),
        "a Java class file"
    );
    // Thin LE 64-bit header: magic, cputype, cpusubtype, filetype.
    let mut exe = vec![0xcdu8; 0];
    exe.extend_from_slice(&[
        0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01, 0, 0, 0, 0, 2, 0, 0, 0,
    ]);
    assert!(macho_is_executable(&exe));
    exe[12] = 6; // MH_DYLIB
    assert!(!macho_is_executable(&exe));

    use crate::macho::fixtures::{
        ID_DYLIB, LOAD_DYLIB, archive, dylib_command, dylib_command32, fat, thin, thin32,
    };
    let files = aterm_tempfile::tempdir().expect("owned keg directory");
    let tool = thin(
        &[
            dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
            dylib_command(LOAD_DYLIB, "@rpath/libfoo.dylib", true),
        ],
        true,
    );
    std::fs::write(files.path().join("tool"), &tool).unwrap();
    assert_eq!(
        linked_dylibs(&files.path().join("tool")).unwrap(),
        ["/usr/lib/libSystem.B.dylib", "@rpath/libfoo.dylib"]
    );
    let lib = thin(
        &[
            dylib_command(ID_DYLIB, "@rpath/libfoo.dylib", false),
            dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", false),
        ],
        false,
    );
    std::fs::write(files.path().join("libfoo"), fat(&[lib.clone(), lib], true)).unwrap();
    assert_eq!(
        linked_dylibs(&files.path().join("libfoo")).unwrap(),
        ["@rpath/libfoo.dylib", "/usr/lib/libSystem.B.dylib"]
    );
    let watch = thin32(
        &[dylib_command32(
            LOAD_DYLIB,
            "/usr/lib/libSystem.B.dylib",
            true,
        )],
        true,
    );
    std::fs::write(files.path().join("watch"), &watch).unwrap();
    assert!(is_macho(&watch[..8]));
    assert_eq!(
        linked_dylibs(&files.path().join("watch")).unwrap(),
        ["/usr/lib/libSystem.B.dylib"]
    );
    std::fs::write(
        files.path().join("libx.a"),
        fat(&[archive(), archive()], false),
    )
    .unwrap();
    assert!(
        linked_dylibs(&files.path().join("libx.a"))
            .unwrap()
            .is_empty()
    );
    std::fs::write(files.path().join("static"), thin(&[], true)).unwrap();
    assert!(
        linked_dylibs(&files.path().join("static"))
            .unwrap()
            .is_empty()
    );
    std::fs::write(files.path().join("cut"), &tool[..40]).unwrap();
    assert!(
        linked_dylibs(&files.path().join("cut"))
            .is_err_and(|e| e.starts_with("inspection failed: malformed"))
    );
    assert!(linked_dylibs(&files.path().join("absent")).is_err());

    let scratch = aterm_tempfile::tempdir().expect("owned library directory");
    let library = scratch.path().join("libok.dylib");
    std::fs::write(&library, b"owned library bytes").unwrap();
    assert!(linked_library_exists(&library));
    assert!(!linked_library_exists(scratch.path()));
    assert!(!linked_library_exists(&scratch.path().join("absent.dylib")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let alias = scratch.path().join("libalias.dylib");
        let dangling = scratch.path().join("libgone.dylib");
        symlink("libok.dylib", &alias).unwrap();
        symlink("absent.dylib", &dangling).unwrap();
        assert!(linked_library_exists(&alias));
        assert!(!linked_library_exists(&dangling));
    }
}

#[test]
fn unpin_splits_like_brew() {
    assert_eq!(unpin_outcome(true, true, false), UnpinOutcome::Remove);
    assert_eq!(unpin_outcome(true, false, false), UnpinOutcome::Remove);
    assert_eq!(
        unpin_outcome(false, true, false),
        UnpinOutcome::WarnNotPinned
    );
    assert_eq!(
        unpin_outcome(false, false, false),
        UnpinOutcome::ErrorNotInstalled
    );
    assert_eq!(unpin_outcome(false, false, true), UnpinOutcome::FailUnknown);

    let url = "https://formulae.brew.sh/api/formula/pkg404.json";
    for code in [Some(22), Some(56)] {
        let missing = fetch::parse_get_response(
            url,
            code,
            b"\n404".to_vec(),
            b"curl: (56) The requested URL returned error: 404",
        )
        .unwrap_err();
        assert!(missing.is_not_found(), "code={code:?}");
        assert_eq!(
            unpin_outcome(false, false, missing.is_not_found()),
            UnpinOutcome::FailUnknown
        );
    }
    for (code, response) in [
        (Some(6), b"\n000".as_slice()),
        (Some(28), b"\n404".as_slice()),
        (Some(22), b"\n500".as_slice()),
        (None, b"\n404".as_slice()),
        (Some(22), b"\n40".as_slice()),
        (Some(22), b"\n40x".as_slice()),
        (Some(22), b"\n404junk".as_slice()),
        (Some(22), b"\n999".as_slice()),
        (Some(22), b"".as_slice()),
    ] {
        let error = fetch::parse_get_response(
            url,
            code,
            response.to_vec(),
            b"transport failure mentioning 404",
        )
        .unwrap_err();
        assert!(!error.is_not_found(), "{code:?} {response:?}");
        assert_eq!(
            unpin_outcome(false, false, error.is_not_found()),
            UnpinOutcome::ErrorNotInstalled
        );
        assert_eq!(
            error.into_message(),
            format!("abrew: download failed ({url}): transport failure mentioning 404")
        );
    }
    assert_eq!(
        fetch::parse_get_response(url, Some(0), b"body\n404\n200".to_vec(), b"").unwrap(),
        b"body\n404"
    );
    assert_eq!(
        fetch::parse_get_response(url, Some(0), vec![0xff, b'\n', b'0', b'0', b'0'], b"").unwrap(),
        [0xff]
    );
    assert!(fetch::parse_get_response(url, Some(0), b"no status".to_vec(), b"").is_err());
}

#[test]
fn outdated_treats_head_kegs_by_their_recorded_stable() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    let keg = p.keg("fzf", "HEAD-abc1234");
    std::fs::create_dir_all(&keg).unwrap();
    let receipt = |stable: &str| {
        format!(
            "{{\"installed_by\":\"abrew\",\"name\":\"fzf\",\"version\":\"HEAD-abc1234\",\"installed_on_request\":\"true\",\"source\":{{\"versions\":{{\"stable\":\"{stable}\",\"version_scheme\":0}}}}}}"
        )
    };
    let f = Formula {
        name: "fzf".into(),
        version: "0.75.0".into(),
        ..Formula::default()
    };
    std::fs::write(keg.join("INSTALL_RECEIPT.json"), receipt("0.75.0")).unwrap();
    let inst = p.installed_one(Kind::Formula, "fzf").unwrap();
    assert!(!formula_outdated(&p, &inst, &f), "stable has not passed");
    std::fs::write(keg.join("INSTALL_RECEIPT.json"), receipt("0.74.0")).unwrap();
    assert!(formula_outdated(&p, &inst, &f), "stable moved past the tab");
}

#[test]
fn cask_outdated_greedy_matrix() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    let inst = |version: &str| super::super::prefix::Installed {
        name: "app".into(),
        version: version.into(),
        versions: vec![version.into()],
        kind: Kind::Cask,
    };
    let cask = |version: &str, auto_updates: bool| super::super::formula::Cask {
        token: "app".into(),
        version: version.into(),
        auto_updates,
        url: "https://example.invalid/app.zip".into(),
        ..Default::default()
    };
    let plain = Greedy::default();
    let greedy = Greedy {
        greedy: true,
        ..Default::default()
    };
    let latest = Greedy {
        latest: true,
        ..Default::default()
    };
    let auto = Greedy {
        auto_updates: true,
        ..Default::default()
    };
    // auto_updates casks need a greedy signal.
    assert!(!cask_outdated(&p, &inst("1.0"), &cask("2.0", true), plain));
    assert!(cask_outdated(&p, &inst("1.0"), &cask("2.0", true), greedy));
    assert!(cask_outdated(&p, &inst("1.0"), &cask("2.0", true), auto));
    assert!(!cask_outdated(&p, &inst("1.0"), &cask("2.0", true), latest));
    // Any mismatch counts, even ahead of the catalog.
    assert!(cask_outdated(&p, &inst("3.0"), &cask("2.0", false), plain));
    assert!(!cask_outdated(
        &p,
        &inst("2.0"),
        &cask("2.0", false),
        greedy
    ));
    // :latest casks need greedy or greedy-latest.
    assert!(!cask_outdated(
        &p,
        &inst("latest"),
        &cask("latest", false),
        plain
    ));
    assert!(cask_outdated(
        &p,
        &inst("latest"),
        &cask("latest", false),
        latest
    ));
    assert!(cask_outdated(
        &p,
        &inst("latest"),
        &cask("latest", false),
        greedy
    ));
}

#[test]
fn outdated_fails_only_for_named_operands() {
    assert!(!outdated_failed(false, false));
    assert!(!outdated_failed(false, true));
    assert!(!outdated_failed(true, false), "bare census reports, rc 0");
    assert!(outdated_failed(true, true), "named + outdated fails");
}

#[test]
fn cask_shard_keys_fonts_by_the_letter_after_font() {
    assert_eq!(cask_shard("alt-tab"), "a");
    assert_eq!(cask_shard("font-monoid"), "font/font-m");
    assert_eq!(cask_shard("font-iosevka-ss09"), "font/font-i");
    assert_eq!(cask_shard("firefox"), "f");
}

#[test]
#[cfg(unix)]
fn long_listing_renders_modes_dates_and_link_targets() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("tool");
    std::fs::write(&file, b"12345").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        mode_string(&std::fs::metadata(&file).unwrap()),
        "-rwxr-xr-x"
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o4750)).unwrap();
    assert_eq!(
        mode_string(&std::fs::metadata(&file).unwrap()),
        "-rwsr-x---"
    );
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1777)).unwrap();
    assert_eq!(mode_string(&std::fs::metadata(&dir).unwrap()), "drwxrwxrwt");
    let link = tmp.path().join("link");
    super::super::prefix::symlink_file(&file, &link).unwrap();
    let mut lister = LongLister::new();
    let line = lister.line(&link);
    let link_mode = mode_string(&std::fs::symlink_metadata(&link).unwrap());
    assert!(link_mode.starts_with('l'), "{link_mode}");
    assert!(line.starts_with(&link_mode), "{line}");
    assert!(line.contains(" -> "), "{line}");
    let line = lister.line(&file);
    assert!(line.contains("       5 "), "{line}");
}

#[test]
fn bundle_dump_to_dev_stdout_prints_instead_of_opening() {
    for name in ["-", "/dev/stdout", "/dev/fd/1"] {
        for force in [false, true] {
            assert_eq!(
                bundle_dump_target(std::path::PathBuf::from(name), force),
                Ok(BundleDumpTarget::Stdout),
                "{name} force={force}"
            );
        }
        assert!(bundle_stdout_path(std::path::Path::new(name)), "{name}");
    }
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("Brewfile");
    assert_eq!(
        bundle_dump_target(file.clone(), false),
        Ok(BundleDumpTarget::File(file.clone()))
    );
    std::fs::write(&file, b"keep").unwrap();
    assert!(bundle_dump_target(file.clone(), false).is_err());
    assert_eq!(
        bundle_dump_target(file.clone(), true),
        Ok(BundleDumpTarget::File(file))
    );
    assert!(!bundle_stdout_path(std::path::Path::new("/dev/stderr")));
    assert_eq!(
        bundle_edit_read(Some(std::path::Path::new("/dev/stdout"))),
        Ok(String::new())
    );
}

#[test]
#[cfg(unix)]
fn bundle_dump_refuses_a_device_without_opening_it() {
    // A FIFO no reader holds tells an open from none: an `O_NONBLOCK` write open of it fails
    // ENXIO, while the refusal before any open says "not a regular file". NEGATIVE CONTROL:
    // without the stat before the open, this failed ENXIO.
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let fifo = tmp.path().join("Brewfile");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `mkfifo` reads the NUL-terminated path `c` owns, alive for the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "the FIFO");
    let err = write_bundle_dump(&fifo, b"x", true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    assert_eq!(err.to_string(), "not a regular file");
    let err = write_bundle_dump(std::path::Path::new("/dev/null"), b"x", true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
}

#[test]
#[cfg(unix)]
fn bundle_dump_reserves_a_new_name_without_following_existing_links() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("Brewfile");
    write_bundle_dump(&file, b"first", false).unwrap();
    assert!(write_bundle_dump(&file, b"second", false).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"first");
    write_bundle_dump(&file, b"forced", true).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"forced");
    let link = tmp.path().join("link");
    super::super::prefix::symlink_file(&file, &link).unwrap();
    assert!(write_bundle_dump(&link, b"changed", false).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"forced");
    let absent = tmp.path().join("absent");
    let dangling = tmp.path().join("dangling");
    super::super::prefix::symlink_file(&absent, &dangling).unwrap();
    assert!(write_bundle_dump(&dangling, b"created", false).is_err());
    assert!(!absent.exists());
    assert_eq!(std::fs::read_link(&dangling).unwrap(), absent);
}

#[test]
fn on_refuses_an_occupied_marker_before_store_or_stub_changes() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    std::fs::create_dir(prefix.disabled_path()).unwrap();
    std::fs::write(prefix.disabled_path().join("owner"), b"keep").unwrap();
    assert_eq!(cmd_on(&prefix), ExitCode::from(1));
    assert_eq!(
        std::fs::read(prefix.disabled_path().join("owner")).unwrap(),
        b"keep"
    );
    assert!(prefix.is_disabled());
}

#[test]
fn doctor_finding_renders_brew_text_and_json_shapes() {
    // Brew Finding#to_s: text, newline, stripped
    // remediation; a list-bodied text keeps its trailing
    // newline, producing the blank line before remediation.
    let f = DoctorFinding::new(append_indented_list(
        &["/x/broken".to_string()],
        "Broken symlinks were found:\n",
    ))
    .with_remediation(
        vec!["brew cleanup".to_string()],
        "Remove them with `brew cleanup`\n",
    );
    assert_eq!(
        f.render(),
        "Broken symlinks were found:\n  /x/broken\n\nRemove them with `brew cleanup`"
    );
    let json = jpretty(&JVal::Obj(vec![
        (
            "tier".into(),
            JVal::Val(aterm_json::Value::Number(1i64.into())),
        ),
        ("findings".into(), JVal::Arr(vec![f.to_jval()])),
    ]));
    assert_eq!(
        json,
        "{\n  \"tier\": 1,\n  \"findings\": [\n    {\n      \"text\": \
         \"Broken symlinks were found:\\n  /x/broken\\n\",\n      \"tier\": 1,\n      \
         \"affects\": [],\n      \"links\": [],\n      \"remediation\": {\n        \
         \"commands\": [\n          \"brew cleanup\"\n        ],\n        \
         \"text\": \"Remove them with `brew cleanup`\\n\"\n      }\n    }\n  ]\n}"
    );
    // No remediation serializes as null; commands-only
    // remediation renders brew's "You can solve" form.
    let bare = DoctorFinding::new("plain");
    assert!(jpretty(&bare.to_jval()).contains("\"remediation\": null"));
    let cmds = DoctorFinding::new("oops\n").with_remediation(vec!["brew fix".to_string()], "");
    assert_eq!(
        cmds.render(),
        "oops\n\nYou can solve this by running:\n  brew fix"
    );
}

#[test]
fn diagnostic_check_groups_print_brew_lines() {
    // Brew Diagnostic.checks: non-fatal findings print via
    // opoo plus a blank line after a multiline finding;
    // fatal ones via ofail with no blank line.
    let multi = || DoctorFinding::new("The following taps are not trusted:\n  probeuser/probe\n");
    let single = || DoctorFinding::new("one liner");
    assert_eq!(
        diagnostic_check_lines(&[multi(), single()], false),
        [
            DiagnosticLine::Warn(
                "Warning: The following taps are not trusted:\n  probeuser/probe".to_string()
            ),
            DiagnosticLine::Blank,
            DiagnosticLine::Warn("Warning: one liner".to_string()),
        ]
    );
    assert_eq!(
        diagnostic_check_lines(&[multi(), single()], true),
        [
            DiagnosticLine::Fail(
                "Error: The following taps are not trusted:\n  probeuser/probe".to_string()
            ),
            DiagnosticLine::Fail("Error: one liner".to_string()),
        ]
    );
    assert!(diagnostic_check_lines(&[], true).is_empty());
}

#[test]
fn doctor_missing_deps_reads_tab_closures_not_declared_graph() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    let keg = prefix.keg("demo", "1");
    std::fs::create_dir_all(&keg).unwrap();
    prefix
        .write_receipt_raw(
            &keg,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "1"),
            ],
            &[("runtime_dependencies", r#"[{"full_name":"present"}]"#)],
        )
        .unwrap();
    owned_keg(&prefix, "present", "1");
    let api = Api::new(prefix.clone());
    let run = |p: &Prefix| {
        cmd_doctor(
            p,
            &api,
            &["check_missing_deps".to_string(), "--quiet".to_string()],
        )
    };
    // The tab's only dep is installed: nothing is missing,
    // whatever the current declared graph says.
    assert_eq!(run(&prefix), ExitCode::SUCCESS);
    prefix
        .write_receipt_raw(
            &keg,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "1"),
            ],
            &[("runtime_dependencies", r#"[{"full_name":"gone"}]"#)],
        )
        .unwrap();
    assert_eq!(run(&prefix), ExitCode::from(1));
}

#[test]
fn cleanup_keeps_old_kegs_until_latest_is_installed() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    prefix.ensure_api_cache().unwrap();
    let api_dir = prefix.cache().join("api");
    std::fs::write(
        api_dir.join("formula-demo.json"),
        br#"{"name":"demo","versions":{"stable":"2"}}"#,
    )
    .unwrap();
    std::fs::write(
        api_dir.join("formula-demo2.json"),
        br#"{"name":"demo2","versions":{"stable":"3"}}"#,
    )
    .unwrap();
    // demo's latest (2) is installed: keg 1 is eligible.
    // demo2's latest (3) is not: both kegs stay (brew's
    // eligible_kegs_for_cleanup gate).
    for version in ["1", "2"] {
        owned_keg(&prefix, "demo", version);
        owned_keg(&prefix, "demo2", version);
    }
    let (removed, _) = cleanup_old_versions(
        &prefix,
        &Api::new(prefix.clone()),
        false,
        true,
        &[],
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(removed, 1);
    assert!(!prefix.keg("demo", "1").exists());
    assert!(prefix.keg("demo", "2").is_dir());
    assert!(prefix.keg("demo2", "1").is_dir());
    assert!(prefix.keg("demo2", "2").is_dir());
}

#[test]
fn abv_counts_files_like_brew() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let dir = temp.path().join("keg");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("only"), b"12345").unwrap();
    assert!(abv(&dir).ends_with('B'));
    assert!(!abv(&dir).contains("files"));
    std::fs::write(dir.join("two"), b"12345").unwrap();
    std::fs::write(dir.join(".DS_Store"), b"12345").unwrap();
    // .DS_Store counts toward size but not the file count.
    assert!(abv(&dir).starts_with("2 files, "), "{}", abv(&dir));
}

/// Unix only: a pin is a symlink, which abrew refuses to make elsewhere.
#[test]
#[cfg(unix)]
fn cleanup_preserves_an_older_pin_foreign_versions_and_other_packages() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    // The wget record's latest (4) is installed, so the
    // record-backed eligibility rule is what runs.
    prefix.ensure_api_cache().unwrap();
    std::fs::write(
        prefix.cache().join("api/formula-wget.json"),
        br#"{"name":"wget","versions":{"stable":"4"}}"#,
    )
    .unwrap();
    for version in ["1", "2"] {
        owned_keg(&prefix, "wget", version);
    }
    prefix.pin("wget").unwrap();
    for version in ["3", "4"] {
        owned_keg(&prefix, "wget", version);
    }
    for version in ["1", "2"] {
        owned_keg(&prefix, "curl", version);
    }
    let foreign = prefix.keg("wget", "999");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("owner"), b"keep").unwrap();
    #[cfg(unix)]
    {
        std::fs::create_dir_all(prefix.bin()).unwrap();
        std::fs::write(prefix.keg("wget", "3").join("tool"), b"x").unwrap();
        std::os::unix::fs::symlink(
            prefix.keg("wget", "3").join("tool"),
            prefix.bin().join("wget-old"),
        )
        .unwrap();
        std::fs::write(prefix.keg("wget", "4").join("tool"), b"current").unwrap();
        std::os::unix::fs::symlink(
            prefix.keg("wget", "3").join("../4/tool"),
            prefix.bin().join("wget-current"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(prefix.bin().join("wget-current")).unwrap(),
            b"current"
        );
        std::os::unix::fs::symlink(
            "../Cellar/wget/3/tool",
            prefix.bin().join("wget-old-relative"),
        )
        .unwrap();
    }
    assert_eq!(
        cleanup_old_versions(
            &prefix,
            &Api::new(prefix.clone()),
            true,
            true,
            &["wget"],
            &mut Vec::new()
        )
        .unwrap()
        .0,
        2
    );
    assert!(prefix.keg("wget", "1").is_dir());
    assert_eq!(
        cleanup_old_versions(
            &prefix,
            &Api::new(prefix.clone()),
            false,
            true,
            &["wget"],
            &mut Vec::new()
        )
        .unwrap()
        .0,
        2
    );
    for version in ["1", "3"] {
        assert!(!prefix.keg("wget", version).exists());
    }
    for version in ["2", "4"] {
        assert!(prefix.keg("wget", version).is_dir());
    }
    assert_eq!(std::fs::read(foreign.join("owner")).unwrap(), b"keep");
    assert!(prefix.keg("curl", "1").is_dir());
    assert_eq!(prefix.pinned_version("wget").as_deref(), Some("2"));
    #[cfg(unix)]
    {
        assert!(std::fs::symlink_metadata(prefix.bin().join("wget-old")).is_err());
        assert!(std::fs::symlink_metadata(prefix.bin().join("wget-old-relative")).is_err());
        assert_eq!(
            std::fs::read_link(prefix.bin().join("wget-current")).unwrap(),
            prefix.keg("wget", "3").join("../4/tool"),
        );
        assert_eq!(
            std::fs::read(prefix.keg("wget", "4").join("tool")).unwrap(),
            b"current"
        );
    }
}

#[test]
fn dep_plan_rows_follow_brew_expansion_order() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    prefix.ensure_api_cache().unwrap();
    let api_dir = prefix.cache().join("api");
    // demo -> alpha -> zeta, nothing installed. Brew's
    // Dependency.expand is post-order (deps before their
    // dependents), so the install group is [zeta, alpha],
    // not alphabetical [alpha, zeta].
    for (name, record) in [
        (
            "demo",
            br#"{"name":"demo","versions":{"stable":"1"},"dependencies":["alpha"]}"#.as_slice(),
        ),
        (
            "alpha",
            br#"{"name":"alpha","versions":{"stable":"1"},"dependencies":["zeta"]}"#.as_slice(),
        ),
        (
            "zeta",
            br#"{"name":"zeta","versions":{"stable":"1"},"dependencies":[]}"#.as_slice(),
        ),
    ] {
        std::fs::write(api_dir.join(format!("formula-{name}.json")), record).unwrap();
    }
    let api = Api::new(prefix.clone());
    let planned = vec![api.formula("demo").unwrap()];
    let exclude: std::collections::BTreeSet<String> = ["demo".to_string()].into_iter().collect();
    let (installs, upgrades) = upgrade_dep_plan(&prefix, &api, &planned, &exclude);
    let names: Vec<&str> = installs.iter().map(|row| row.0.as_str()).collect();
    assert_eq!(names, ["zeta", "alpha"]);
    assert!(upgrades.is_empty());
}

#[test]
fn fetch_require_sha_env_fires_on_presence() {
    let expected = Some(
        "Error: Calling HOMEBREW_CASK_OPTS_REQUIRE_SHA is disabled! Use HOMEBREW_CASK_OPTS instead."
            .to_string(),
    );
    assert_eq!(fetch_require_sha_env_error(|_| Some("1".into())), expected);
    assert_eq!(fetch_require_sha_env_error(|_| Some("0".into())), expected);
    assert_eq!(fetch_require_sha_env_error(|_| Some(String::new())), None);
    assert_eq!(fetch_require_sha_env_error(|_| None), None);
}

#[test]
fn info_json_enrichment_reports_installed_state() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    prefix.ensure_api_cache().unwrap();
    let api_dir = prefix.cache().join("api");
    std::fs::write(
        api_dir.join("formula-demo.json"),
        br#"{"name":"demo","versions":{"stable":"2"},"dependencies":["dep"]}"#,
    )
    .unwrap();
    std::fs::write(
        api_dir.join("formula-dep.json"),
        br#"{"name":"dep","versions":{"stable":"3"},"compatibility_version":1,"bottle":{"stable":{"rebuild":2,"files":{"all":{"url":"https://example.invalid/dep.bottle.tar.gz","sha256":"abc","cellar":":any"}}}}}"#,
    )
    .unwrap();
    let keg = prefix.keg("demo", "1");
    std::fs::create_dir_all(&keg).unwrap();
    prefix
        .write_receipt(
            &keg,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "1"),
                ("installed_on_request", "true"),
                ("full_name", "demo"),
            ],
        )
        .unwrap();
    owned_keg(&prefix, "dep", "3_1");
    let api = Api::new(prefix.clone());
    let mut record = aterm_json::json!({"name": "demo"});
    enrich_formula_json(&prefix, &api, &mut record);
    // Brew's API-mode to_hash: requirements empty, patches untyped.
    let mut rich = aterm_json::json!({
        "name": "demo",
        "requirements": [{"name": "macos", "version": "12"}],
        "github_packages_downloads": {"source": "https://example.invalid/x"},
        "service": {"run": ["$HOMEBREW_PREFIX/opt/demo/bin/demo"], "working_dir": "$HOMEBREW_PREFIX"},
        "patches": [{"strip": "p1", "url": "https://x/y.diff", "type": "backport"}]
    });
    enrich_formula_json(&prefix, &api, &mut rich);
    assert_eq!(rich.get("requirements"), Some(&aterm_json::json!([])));
    assert!(rich.get("github_packages_downloads").is_none());
    let run0 = rich.pointer("/service/run/0").unwrap().as_str().unwrap();
    assert!(
        !run0.contains("$HOMEBREW_PREFIX") && run0.ends_with("/opt/demo/bin/demo"),
        "{run0}"
    );
    assert!(
        rich.get("patches").unwrap()[0].get("type").is_none(),
        "patch type dropped like brew's API loader"
    );
    // Cask side: placeholders expand, raw-API-only keys drop.
    let mut cask = aterm_json::json!({
        "token": "demo-cask",
        "version": "1",
        "language_variations": [{"languages": ["en"]}],
        "artifacts": [{"binary": ["$APPDIR/Demo.app/tool"], "target": "$HOMEBREW_PREFIX/bin/tool"}]
    });
    enrich_cask_json(&prefix, &mut cask);
    assert!(
        cask.get("language_variations").is_none(),
        "brew's cask to_h has no language_variations key"
    );
    let arts = cask.get("artifacts").unwrap().to_string();
    assert!(
        !arts.contains("$APPDIR") && !arts.contains("$HOMEBREW_PREFIX"),
        "{arts}"
    );
    let installed = record
        .get("installed")
        .and_then(aterm_json::Value::as_array)
        .unwrap();
    assert_eq!(installed.len(), 1);
    let row = &installed[0];
    assert_eq!(row.get("version").unwrap(), &aterm_json::json!("1"));
    assert_eq!(
        row.get("installed_on_request").unwrap(),
        &aterm_json::json!(true)
    );
    assert!(row.get("time").is_some_and(|t| !t.is_null()));
    let runtime = row
        .get("runtime_dependencies")
        .and_then(aterm_json::Value::as_array)
        .unwrap();
    assert_eq!(runtime.len(), 1);
    assert_eq!(
        runtime[0].get("full_name").unwrap(),
        &aterm_json::json!("dep")
    );
    // Brew's full dep-hash row: version/revision split from
    // the installed pkg_version, rebuild + compatibility
    // from the dep's formula.
    assert_eq!(runtime[0].get("version").unwrap(), &aterm_json::json!("3"));
    assert_eq!(runtime[0].get("revision").unwrap(), &aterm_json::json!(1));
    assert_eq!(
        runtime[0].get("bottle_rebuild").unwrap(),
        &aterm_json::json!(2)
    );
    assert_eq!(
        runtime[0].get("pkg_version").unwrap(),
        &aterm_json::json!("3_1")
    );
    assert_eq!(
        runtime[0].get("declared_directly").unwrap(),
        &aterm_json::json!(true)
    );
    assert_eq!(
        runtime[0].get("compatibility_version").unwrap(),
        &aterm_json::json!(1)
    );
    assert!(record.get("linked_keg").is_some_and(|v| v.is_null()));
    assert_eq!(record.get("pinned").unwrap(), &aterm_json::json!(false));
    assert_eq!(record.get("outdated").unwrap(), &aterm_json::json!(true));
    let mut absent = aterm_json::json!({"name": "dep"});
    enrich_formula_json(&prefix, &api, &mut absent);
    assert_eq!(
        absent
            .get("installed")
            .and_then(aterm_json::Value::as_array)
            .unwrap()
            .len(),
        1,
        "dep itself is installed"
    );
}

#[test]
fn disk_usage_readable_matches_brew_units() {
    assert_eq!(disk_usage_readable(0), "0B");
    assert_eq!(disk_usage_readable(999), "999B");
    assert_eq!(disk_usage_readable(1_000), "1KB");
    assert_eq!(disk_usage_readable(1_500), "1.5KB");
    assert_eq!(disk_usage_readable(193_300), "193.3KB");
    assert_eq!(disk_usage_readable(1_000_000), "1MB");
    assert_eq!(disk_usage_readable(1_050_000), "1.1MB");
    assert_eq!(disk_usage_readable(2_500_000_000), "2.5GB");
}

#[test]
fn cask_bundle_versions_read_the_staged_app_plist() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cask");
    let contents = dir.join("Widget.app/Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        "<?xml version=\"1.0\"?><plist><dict>\
         <key>CFBundleShortVersionString</key><string>2.5 (409)</string>\
         <key>CFBundleVersion</key><string>409</string>\
         </dict></plist>",
    )
    .unwrap();
    // Brew strips the trailing " (<version>)" from the short form.
    assert_eq!(
        cask_bundle_versions(&dir),
        (Some("2.5".to_string()), Some("409".to_string()))
    );
    assert_eq!(cask_bundle_versions(tmp.path()), (None, None));
}

#[test]
fn fuzzy_dictionary_includes_tap_directory_names() {
    // Brew's fuzzy dictionary is Set(names + full_names):
    // sorted short names (core + tap leaves), then sorted
    // qualified tap names — plain tap directories count,
    // no .git, no trust gate.
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let probe = p
        .taps_dir()
        .join("ProbeUser")
        .join("homebrew-probe")
        .join("Formula");
    std::fs::create_dir_all(&probe).expect("mkdir");
    std::fs::write(probe.join("custool.rb"), "# f\n").expect("write");
    std::fs::write(probe.join("wget.rb"), "# f\n").expect("write");
    let api = Api::read_only(p.clone());
    let core = vec!["wget".to_string(), "cuetools".to_string()];
    let dict = fuzzy_dictionary(&api, &core);
    assert_eq!(
        dict,
        [
            "cuetools",
            "custool",
            "wget",
            "probeuser/probe/custool",
            "probeuser/probe/wget"
        ]
    );
    assert_eq!(spell_correct(&dict, "custol"), ["custool"]);
    // Cask token dictionary: core tokens are present; a
    // tap's tokens join only when that tap is trusted,
    // which is machine state this test does not assume.
    let casks = p
        .taps_dir()
        .join("ProbeUser")
        .join("homebrew-probe")
        .join("Casks");
    std::fs::create_dir_all(&casks).expect("mkdir");
    std::fs::write(casks.join("custapp.rb"), "# c\n").expect("write");
    let dict = cask_token_dictionary(&api, &["caffeine".to_string()]);
    assert!(dict.contains(&"caffeine".to_string()), "{dict:?}");
}

#[test]
fn spell_correct_tie_order_matches_cruby_uniform_sort() {
    // Gate sets and outcomes measured from brew's own Ruby
    // (DidYouMean::SpellChecker over the live dictionary);
    // the tie order is CRuby 4.0's uniform introsort.
    let dict: Vec<String> = [
        "mpi4py",
        "optipng",
        "pbzip2",
        "pcp",
        "periphery",
        "php",
        "pick",
        "pict",
        "pie",
        "pig",
        "pigz",
        "pike",
        "pint",
        "pip-audit",
        "pip-completion",
        "pip-tools",
        "pipdeptree",
        "pipe-rename",
        "pipebench",
        "pipelight",
        "pipemeter",
        "pipenv",
        "pipes-sh",
        "pipet",
        "pipewire",
        "pipewire-gstreamer",
        "pipgrip",
        "pipx",
        "pit",
        "pius",
        "pivy",
        "pixd",
        "pixi",
        "pixi-pack",
        "pixz",
        "pop",
        "prips",
        "pup",
        "pyp",
        "sip",
        "spiped",
        "vip",
        "xpipe",
        "zip",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        spell_correct(&dict, "pip"),
        [
            "pipx", "pit", "pie", "pig", "pop", "pup", "pcp", "pyp", "php", "sip", "vip", "zip"
        ]
    );
    let dict: Vec<String> = [
        "gfxutil", "gputils", "gti", "guile", "gut", "psutils", "suil",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        spell_correct(&dict, "gsutil"),
        ["suil", "gputils", "gfxutil", "psutils"]
    );
    // Tie-heavy key batteries: the raw permutation must
    // match Ruby's sort_by index for index.
    let keys = [
        f64::from_bits(4600427019358961664),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4598175219545276416),
        f64::from_bits(0),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4602678819172646912),
        f64::from_bits(0),
        f64::from_bits(4600427019358961664),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4600427019358961664),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4604930618986332160),
        f64::from_bits(0),
        f64::from_bits(0),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4606056518893174784),
        f64::from_bits(0),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4600427019358961664),
        f64::from_bits(4600427019358961664),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4602678819172646912),
        f64::from_bits(0),
        f64::from_bits(4603804719079489536),
        f64::from_bits(0),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4603804719079489536),
        f64::from_bits(0),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4603804719079489536),
        f64::from_bits(0),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4606056518893174784),
        f64::from_bits(0),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4603804719079489536),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4602678819172646912),
        f64::from_bits(0),
        f64::from_bits(4593671619917905920),
        f64::from_bits(4593671619917905920),
        f64::from_bits(0),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4604930618986332160),
        f64::from_bits(4602678819172646912),
        f64::from_bits(4598175219545276416),
        f64::from_bits(4606056518893174784),
        f64::from_bits(4600427019358961664),
        f64::from_bits(4606056518893174784),
        f64::from_bits(0),
    ];
    assert_eq!(
        ruby_uniform_sort_permutation(&keys),
        [
            67, 71, 17, 77, 53, 33, 87, 34, 90, 51, 37, 4, 99, 35, 38, 31, 39, 25, 47, 61, 42, 21,
            72, 74, 6, 89, 80, 84, 88, 44, 1, 3, 95, 92, 85, 30, 55, 28, 56, 26, 24, 57, 97, 46,
            45, 0, 18, 20, 94, 9, 86, 78, 15, 16, 73, 69, 40, 64, 58, 48, 49, 50, 54, 43, 5, 10,
            11, 82, 14, 52, 75, 41, 62, 70, 66, 63, 8, 91, 7, 93, 83, 22, 79, 68, 2, 32, 60, 96,
            98, 59, 23, 65, 12, 13, 81, 27, 36, 76, 29, 19
        ]
    );
    let keys = [
        f64::from_bits(4616189618054758400),
        f64::from_bits(4607182418800017408),
        f64::from_bits(4613937818241073152),
        f64::from_bits(4613937818241073152),
        f64::from_bits(4616189618054758400),
        f64::from_bits(4607182418800017408),
        f64::from_bits(0),
        f64::from_bits(4607182418800017408),
        f64::from_bits(4611686018427387904),
        f64::from_bits(4611686018427387904),
        f64::from_bits(0),
        f64::from_bits(4616189618054758400),
        f64::from_bits(0),
        f64::from_bits(4616189618054758400),
        f64::from_bits(0),
        f64::from_bits(4613937818241073152),
        f64::from_bits(4611686018427387904),
        f64::from_bits(4613937818241073152),
        f64::from_bits(4616189618054758400),
        f64::from_bits(4616189618054758400),
        f64::from_bits(0),
        f64::from_bits(0),
        f64::from_bits(0),
        f64::from_bits(4613937818241073152),
        f64::from_bits(0),
        f64::from_bits(4611686018427387904),
        f64::from_bits(0),
        f64::from_bits(4607182418800017408),
        f64::from_bits(4616189618054758400),
        f64::from_bits(4607182418800017408),
        f64::from_bits(4613937818241073152),
        f64::from_bits(4613937818241073152),
        f64::from_bits(0),
        f64::from_bits(4613937818241073152),
        f64::from_bits(0),
        f64::from_bits(4607182418800017408),
        f64::from_bits(0),
        f64::from_bits(0),
        f64::from_bits(4616189618054758400),
        f64::from_bits(4616189618054758400),
    ];
    assert_eq!(
        ruby_uniform_sort_permutation(&keys),
        [
            34, 20, 21, 22, 14, 36, 12, 37, 10, 24, 6, 26, 32, 7, 5, 27, 29, 1, 35, 25, 16, 9, 8,
            17, 15, 23, 3, 33, 2, 30, 31, 38, 39, 28, 19, 18, 13, 11, 4, 0
        ]
    );
}

#[test]
fn spell_correct_matches_did_you_mean() {
    let dict: Vec<String> = ["zsxd", "zstd", "firefox", "firefly", "hello"]
        .iter()
        .map(ToString::to_string)
        .collect();
    // The oracle's own outcomes: `brew search zstd` also finds
    // zsxd, and `brew search firefx` finds firefox (exact input
    // is never its own correction).
    assert_eq!(spell_correct(&dict, "zstd"), ["zsxd"]);
    assert_eq!(spell_correct(&dict, "firefx"), ["firefox", "firefly"]);
    assert!(spell_correct(&dict, "zzzzzz").is_empty());
    let chars = |s: &str| s.chars().collect::<Vec<_>>();
    assert_eq!(levenshtein_distance(&chars("kitten"), &chars("sitting")), 3);
    assert!(jaro_winkler_distance(&chars("zsxd"), &chars("zstd")) >= 0.834);
    let sentence = |items: &[&str]| {
        to_sentence_or(&items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    };
    assert_eq!(sentence(&[]), "");
    assert_eq!(sentence(&["wget"]), "wget");
    assert_eq!(sentence(&["wget", "wgo"]), "wget or wgo");
    assert_eq!(
        sentence(&["wget", "wgo", "vgt", "ngt"]),
        "wget, wgo, vgt or ngt"
    );
    assert_eq!(
        cask_unavailable("nosuch"),
        "Cask 'nosuch' is unavailable: No Cask with this name exists."
    );
}

#[test]
fn migration_lookup_fetches_only_the_sources_it_needs() {
    let dir = crate::testdir::dir("abrew-migration-sources-");
    let p = Prefix::at(&dir.join("prefix"));
    p.ensure().unwrap();
    for hit in [Some(false), Some(true), None] {
        let mut calls = Vec::new();
        let reason = tap_migration_reason_with(&p, "oldthing", |casks| {
            calls.push(casks);
            if hit == Some(casks) {
                std::collections::BTreeMap::from([("oldthing".into(), "newthing".into())])
            } else {
                Default::default()
            }
        });
        assert_eq!(reason.is_some(), hit.is_some());
        assert_eq!(
            calls,
            if hit == Some(false) {
                vec![false]
            } else {
                vec![false, true]
            }
        );
    }
    std::fs::create_dir_all(p.taps_dir().join("homebrew/homebrew-core/.git")).unwrap();
    let mut calls = Vec::new();
    assert!(
        tap_migration_reason_with(&p, "oldthing", |casks| {
            calls.push(casks);
            Default::default()
        })
        .is_none()
    );
    assert_eq!(calls, [true], "an installed core tap supplies its own map");
}

#[test]
fn missing_formula_reasons_match_brew() {
    assert_eq!(
        disallowed_reason("pip").as_deref(),
        Some("pip is part of the python formula:\n  brew install python\n")
    );
    assert!(disallowed_reason("rubygems").is_some_and(|r| r.contains("brew install ruby")));
    assert!(disallowed_reason("liblzma").is_some_and(|r| r.contains("brew install xz")));
    assert!(disallowed_reason("wget").is_none());

    // Migration message shapes from a fabricated tap map.
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let tap_dir = p.taps_dir().join("me").join("homebrew-mine");
    std::fs::create_dir_all(tap_dir.join(".git")).expect("mkdir");
    std::fs::write(
        tap_dir.join("tap_migrations.json"),
        r#"{"oldthing": "homebrew/cask/newthing"}"#,
    )
    .expect("write");
    let reason = tap_migration_reason_with(&p, "oldthing", |_| {
        panic!("a known local migration must not request core metadata")
    })
    .expect("reason");
    assert_eq!(
        reason,
        "It was migrated from me/mine to homebrew/cask/newthing.\n\
         You can access it again by running:\n  brew tap homebrew/cask\n\
         And then you can install it by running:\n  brew install --cask newthing\n"
    );
}

#[cfg(unix)]
#[test]
fn deleted_reason_reads_core_tap_history() {
    // A core tap whose history deletes a formula inside the
    // last month: brew's deleted_reason quotes the commit.
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let tap_dir = p.taps_dir().join("homebrew").join("homebrew-core");
    std::fs::create_dir_all(tap_dir.join("Formula/o")).expect("mkdir");
    let run = |args: &[&str]| {
        let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
            .arg("-C")
            .arg(&tap_dir)
            .args(["-c", "user.email=a@b.c", "-c", "user.name=A"])
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    };
    run(&["init", "-q"]);
    std::fs::write(tap_dir.join("Formula/o/oldgone.rb"), "# f\n").expect("write");
    run(&["add", "."]);
    run(&["commit", "-qm", "oldgone 1.0"]);
    // A still-present formula never gets a deleted reason.
    assert!(deleted_reason_mode(&p, "oldgone", true).is_none());
    run(&["rm", "-q", "Formula/o/oldgone.rb"]);
    run(&["commit", "-qm", "oldgone: remove formula"]);
    let reason = deleted_reason_mode(&p, "oldgone", true).expect("deleted reason");
    assert!(
        reason.contains("oldgone was deleted from homebrew/core in commit "),
        "{reason}"
    );
    assert!(reason.contains("oldgone: remove formula"), "{reason}");
    assert!(
        reason.contains("git -C \"$(brew --repo homebrew/core)\" show "),
        "{reason}"
    );
    assert!(reason.contains(":Formula/o/oldgone.rb"), "{reason}");
    // Unknown name: no deletion in the window, no reason.
    assert!(deleted_reason_mode(&p, "neverwas", true).is_none());
}

#[cfg(unix)]
#[test]
fn prune_dry_run_propagates_emptiness() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let nested = p.root.join("share/empty/nested");
    std::fs::create_dir_all(&nested).expect("mkdir");
    let onlylink = p.root.join("share/onlylink");
    std::fs::create_dir_all(&onlylink).expect("mkdir");
    std::os::unix::fs::symlink("/nonexistent/abrew-r319", onlylink.join("dead")).expect("symlink");
    let mut lines: Vec<String> = Vec::new();
    prune_prefix_links(&p, true, &mut lines);
    let text = lines.join("\n");
    assert!(text.contains("Would remove (broken link):"), "{text}");
    assert!(
        text.contains(&format!(
            "Would remove (empty directory): {}",
            onlylink.display()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "Would remove (empty directory): {}",
            p.root.join("share/empty").display()
        )),
        "{text}"
    );
    // Dry run removed nothing.
    assert!(
        nested.is_dir()
            && std::fs::symlink_metadata(onlylink.join("dead")).is_ok_and(|m| m.is_symlink())
    );
}

#[test]
fn doctor_and_cleanup_see_broken_links() {
    assert!(DOCTOR_CHECKS.contains(&"check_for_broken_symlinks"));
    #[cfg(unix)]
    {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let bin = p.root.join("bin");
        std::os::unix::fs::symlink("/nonexistent/abrew-r134-target", bin.join("dead"))
            .expect("symlink");
        let deep = p.root.join("share/man/man1");
        std::fs::create_dir_all(&deep).expect("mkdir");
        std::os::unix::fs::symlink("/nonexistent/abrew-r134-target", deep.join("dead.1"))
            .expect("symlink");
        std::os::unix::fs::symlink("/bin/sh", bin.join("alive")).expect("symlink");
        let broken = broken_prefix_links(&p);
        assert_eq!(broken.len(), 2, "bin and share broken links");
        assert!(broken.iter().all(|b| b.file_name().unwrap() != "alive"));
        prune_prefix_links(&p, false, &mut Vec::new());
        assert!(
            broken_prefix_links(&p).is_empty(),
            "cleanup prune removed them"
        );
        assert!(bin.join("alive").exists(), "live link kept");
        assert!(bin.is_dir(), "anchor kept");
    }
}

#[test]
fn tap_info_pieces_match_brew() {
    assert_eq!(pluralize_with_count("formula", 1), "1 formula");
    assert_eq!(pluralize_with_count("formula", 3), "3 formulae");
    assert_eq!(pluralize_with_count("tap", 0), "0 taps");
    assert_eq!(pluralize_with_count("command", 2), "2 commands");

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let tap_path = dir.path().join("homebrew-testtap");
    for sub in ["Formula/w", "Casks", "cmd"] {
        std::fs::create_dir_all(tap_path.join(sub)).expect("mkdir");
    }
    std::fs::write(tap_path.join("Formula/w/wget.rb"), "#").expect("write");
    std::fs::write(tap_path.join("Formula/hello.rb"), "#").expect("write");
    std::fs::write(tap_path.join("Casks/firefox.rb"), "#").expect("write");
    std::fs::write(tap_path.join("cmd/brew-hello-cmd.rb"), "#").expect("write");
    let tap = tap::Tap {
        user: "me".to_string(),
        repo: "testtap".to_string(),
        path: tap_path,
    };
    assert_eq!(tap_formula_names(&tap), vec!["hello", "wget"]);
    assert_eq!(tap_cask_tokens(&tap), vec!["firefox"]);
    assert_eq!(tap_command_names(&tap), vec!["hello-cmd"]);
    assert_eq!(
        tap_contents(&tap),
        vec!["1 command", "1 cask", "2 formulae"]
    );
}

#[test]
fn ls_columns_matches_bsd_ls() {
    // Oracle: `/bin/ls` of these names under a pty (this host).
    let names: Vec<String> = [
        "a",
        "bb",
        "cccccccccccccccccc",
        "ddd",
        "ee",
        "ffffffffff",
        "gg",
        "h",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        ls_columns(&names, 80),
        "a\t\t\tddd\t\t\tgg\nbb\t\t\tee\t\t\th\ncccccccccccccccccc\tffffffffff\n"
    );
    // One column when the tab-rounded width leaves room for only one.
    assert_eq!(
        ls_columns(&names, 24),
        "a\nbb\ncccccccccccccccccc\nddd\nee\nffffffffff\ngg\nh\n"
    );
    assert_eq!(ls_columns(&[], 80), "");
}

#[test]
fn format_columns_matches_brews_layout() {
    let names: Vec<String> = ["a", "bb", "ccc", "dd", "e"]
        .iter()
        .map(ToString::to_string)
        .collect();
    // Oracle: brew's Formatter.columns at width 12 (pty-captured).
    assert_eq!(format_columns(&names, 12, true), "a      dd\nbb     e\nccc");
    // Piped output is always one per line, at any width.
    assert_eq!(format_columns(&names, 80, false), "a\nbb\nccc\ndd\ne");
    // Too narrow for two columns falls back to lines.
    assert_eq!(format_columns(&names, 4, true), "a\nbb\nccc\ndd\ne");
}

#[test]
fn dry_change_lines_render_install_and_upgrade_rows() {
    let fresh = |n: &str, v: &str| install::DryRow {
        name: n.into(),
        old_version: None,
        new_version: v.into(),
    };
    // Oracle sample (brew install --dry-run): one space, name then
    // the new version alone.
    assert_eq!(
        dry_change_lines(&[fresh("wget", "1.25.0_2")]),
        ["wget 1.25.0_2"]
    );
    assert_eq!(
        dry_change_lines(&[fresh("libpsl", "0.23.3")]),
        ["libpsl 0.23.3"]
    );
    let upgraded = install::DryRow {
        name: "ca-certificates".into(),
        old_version: Some("2026-09-25_1".into()),
        new_version: "2026-10-20".into(),
    };
    assert_eq!(
        dry_change_lines(&[upgraded]),
        ["ca-certificates 2026-09-25_1 -> 2026-10-20"]
    );
}

#[test]
fn version_change_lines_align_like_brew() {
    let row = |n: &str, o: &str, v: &str, s: Option<&str>| {
        (n.into(), o.into(), v.into(), s.map(str::to_owned))
    };
    // Oracle sample (brew upgrade --dry-run, redirected).
    let lines = version_change_lines(&[
        row("brotli", "1.2.0", "1.2.1", Some("791.9KB")),
        row(
            "ca-certificates",
            "2026-09-25_1",
            "2026-10-20",
            Some("109.6KB"),
        ),
        row("gh", "2.102.0", "2.103.0", None),
    ]);
    assert_eq!(
        lines,
        [
            "brotli           1.2.0        -> 1.2.1       (791.9KB)",
            "ca-certificates  2026-09-25_1 -> 2026-10-20  (109.6KB)",
            "gh               2.102.0      -> 2.103.0",
        ]
    );
    // A single change joins with one space.
    assert_eq!(
        version_change_lines(&[row("xz", "5.8.3", "5.8.4", None)]),
        ["xz 5.8.3 -> 5.8.4"]
    );
}

#[test]
fn repo_url_extraction_matches_brew() {
    let some = |s: &str| Some(s.to_string());
    assert_eq!(
        repo_url_of("https://github.com/cli/cli.git"),
        some("https://github.com/cli/cli")
    );
    assert_eq!(
        repo_url_of("https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq.tar.gz"),
        some("https://github.com/jqlang/jq")
    );
    assert_eq!(
        repo_url_of("https://gitlab.com/group/sub/repo/-/archive/1.0/repo-1.0.tar.gz"),
        some("https://gitlab.com/group/sub/repo")
    );
    // A bare GitLab path has none of brew's required markers.
    assert_eq!(repo_url_of("https://gitlab.com/group/repo"), None);
    assert_eq!(
        repo_url_of("https://bitbucket.org/atlassian/localstack/get/1.0.tar.gz"),
        some("https://bitbucket.org/atlassian/localstack")
    );
    assert_eq!(
        repo_url_of("https://codeberg.org/dnkl/foot/archive/1.16.2.tar.gz"),
        some("https://codeberg.org/dnkl/foot")
    );
    assert_eq!(
        repo_url_of("https://git.sr.ht/~sircmpwn/hut/archive/v1.0.0.tar.gz"),
        some("https://sr.ht/~sircmpwn/hut")
    );
    assert_eq!(
        repo_url_of("https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz"),
        None
    );
    assert_eq!(repo_url_of("https://example.com/u/r"), None);
}

#[test]
fn der_walk_extracts_subject_cn() {
    // Certificate SEQ { TBS SEQ { [0] {int}, int, SEQ, SEQ,
    // SEQ, SEQ { SET { SEQ { OID CN, UTF8 "Test Root" } } } } }
    fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag, body.len() as u8];
        out.extend_from_slice(body);
        out
    }
    let atv = tlv(
        0x30,
        &[tlv(0x06, &[0x55, 0x04, 0x03]), tlv(0x0c, b"Test Root")].concat(),
    );
    let subject = tlv(0x30, &tlv(0x31, &atv));
    let tbs = tlv(
        0x30,
        &[
            tlv(0xa0, &tlv(0x02, &[2])),
            tlv(0x02, &[1]),
            tlv(0x30, &tlv(0x06, &[0x2a])),
            tlv(0x30, &[]),
            tlv(0x30, &[]),
            subject,
        ]
        .concat(),
    );
    let cert = tlv(0x30, &tbs);
    assert_eq!(super::cert_subject_cns(&cert), ["Test Root"]);
}

#[test]
fn dotted_version_and_origin_config() {
    use std::cmp::Ordering;
    assert_eq!(
        super::dotted_version_cmp("2.30.0", "2.9.9"),
        Ordering::Greater
    );
    assert_eq!(super::dotted_version_cmp("2.30", "2.30.0"), Ordering::Equal);
    assert_eq!(super::dotted_version_cmp("2.4.1", "2.30.0"), Ordering::Less);
    let dir = aterm_tempfile::tempdir().unwrap();
    let git = dir.path().join(".git");
    std::fs::create_dir_all(&git).unwrap();
    std::fs::write(
        git.join("config"),
        "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = https://github.com/Homebrew/brew.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
    )
    .unwrap();
    assert_eq!(
        super::origin_url_from_config(dir.path()).as_deref(),
        Some("https://github.com/Homebrew/brew.git")
    );
    std::fs::write(
        git.join("config"),
        "[include]\n\tpath = other\n[remote \"origin\"]\n\turl = https://example.invalid/x\n",
    )
    .unwrap();
    assert_eq!(super::origin_url_from_config(dir.path()), None);
}

#[test]
fn stray_files_skip_dotfiles_and_nonrecursive_globs() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("libkeep.dylib"), b"x").unwrap();
    std::fs::write(root.join(".hidden.dylib"), b"x").unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/libdeep.dylib"), b"x").unwrap();
    std::fs::create_dir_all(root.join(".hdir")).unwrap();
    std::fs::write(root.join(".hdir/libsecret.dylib"), b"x").unwrap();
    let root = root.display().to_string();
    assert_eq!(
        super::stray_files(&root, "*.dylib", &[], false),
        [format!("{root}/libkeep.dylib")]
    );
    assert_eq!(
        super::stray_files(&root, "**/*.dylib", &[], true),
        [
            format!("{root}/libkeep.dylib"),
            format!("{root}/sub/libdeep.dylib"),
        ]
    );
}

#[test]
fn formula_class_s_and_rb_class_scan() {
    assert_eq!(super::formula_class_s("toolx"), "Toolx");
    assert_eq!(super::formula_class_s("openssl@3"), "OpensslAT3");
    assert_eq!(super::formula_class_s("gtk+"), "Gtkx");
    assert_eq!(super::formula_class_s("foo-bar_baz.qux"), "FooBarBazQux");
    let (all, formulae) = super::rb_class_constants(
        "class WrongName < Formula\nend\nclass Helper\nend\nclass Ns::Deep < Formula\nend\n",
    );
    assert_eq!(all, ["WrongName", "Helper", "Deep"]);
    assert_eq!(formulae, ["WrongName", "Deep"]);
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn bundle_exec_invocation_and_computed_env_are_separate() {
    let rest = vec![
        "exec".to_string(),
        "--file=/tmp/Brewfile".to_string(),
        "/usr/bin/printf".to_string(),
        "--check".to_string(),
        "--install".to_string(),
        "--no-secrets".to_string(),
        "-vq".to_string(),
        "token with spaces".to_string(),
    ];
    let invocation = super::bundle_exec_invocation(&rest, false).unwrap();
    assert_eq!(
        invocation.command,
        [
            "/usr/bin/printf",
            "--check",
            "--install",
            "--no-secrets",
            "-vq",
            "token with spaces",
        ]
    );
    assert!(!invocation.install);

    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = super::super::prefix::Prefix::at(temp.path());
    prefix.ensure().unwrap();
    let api = super::super::api::Api::new(prefix.clone());
    let computed = super::bundle_computed_env(&prefix, &api, &[], false).unwrap();
    if let Some(path) = computed.old_env.get("PATH") {
        assert_eq!(computed.env.get("HOMEBREW_PATH"), Some(path));
    }
    assert!(computed.env.get("PATH").is_some_and(|p| !p.is_empty()));
    assert_eq!(
        computed.env.get("HOMEBREW_PREFIX").map(String::as_str),
        Some(prefix.root.to_string_lossy().as_ref())
    );
    assert_eq!(
        computed.env.get("HOMEBREW_CELLAR").map(String::as_str),
        Some(prefix.cellar().to_string_lossy().as_ref())
    );

    let mut shell_env = std::collections::BTreeMap::new();
    shell_env.insert("SHELL".to_string(), "/bin/sh".to_string());
    shell_env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    shell_env.insert(
        "HOMEBREW_FORCE_API_AUTO_UPDATE".to_string(),
        "1".to_string(),
    );
    let prepared = super::bundle_shell_command(&prefix, shell_env).unwrap();
    let (command, shell_env) = (&prepared.command, &prepared.env);
    assert_eq!(command.as_slice(), ["/bin/sh"]);
    assert!(!shell_env.contains_key("HOMEBREW_FORCE_API_AUTO_UPDATE"));
    assert!(!shell_env.contains_key("BREW_PROMPT_PATH"));
    assert_eq!(
        shell_env.get("PS1").map(String::as_str),
        Some("\\[\\033[1;32m\\]brew bundle \\[\\033[1;31m\\]\\w \\[\\033[1;34m\\]$\\[\\033[0m\\] ")
    );

    #[cfg(unix)]
    {
        let mut env = std::collections::BTreeMap::new();
        env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        env.insert(
            "BUNDLE_R355_TEST".to_string(),
            "value with spaces".to_string(),
        );
        let command = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "test \"$BUNDLE_R355_TEST\" = 'value with spaces'".to_string(),
        ];
        assert_eq!(
            super::run_bundle_command(&command, env),
            std::process::ExitCode::SUCCESS
        );
    }
}

#[test]
fn glob_match_follows_dir_glob_rules() {
    assert!(super::glob_match("*.dylib", "libfoo.dylib"));
    assert!(!super::glob_match("*.dylib", "sub/libfoo.dylib"));
    assert!(super::glob_match("**/*.h", "fuse/old/fuse.h"));
    assert!(super::glob_match("**/*.h", "top.h"));
    assert!(super::glob_match("fuse/**/*.h", "fuse/fuse.h"));
    assert!(!super::glob_match("fuse/**/*.h", "other/fuse.h"));
    assert!(super::glob_match("libsymsea*.dylib", "libsymsea-1.dylib"));
}

#[test]
fn ruby_str_inspect_quotes_like_ruby() {
    assert_eq!(super::ruby_str_inspect("/tmp/x"), "\"/tmp/x\"");
    assert_eq!(super::ruby_str_inspect("a\"b\\c"), "\"a\\\"b\\\\c\"");
    assert_eq!(super::ruby_str_inspect("a\nb"), "\"a\\nb\"");
}

#[test]
fn utils_text_to_sentence_drops_oxford_comma() {
    assert_eq!(super::utils_text_to_sentence(&[]), "");
    assert_eq!(super::utils_text_to_sentence(&["a"]), "a");
    assert_eq!(super::utils_text_to_sentence(&["a", "b"]), "a and b");
    assert_eq!(
        super::utils_text_to_sentence(&["formulae", "casks", "commands"]),
        "formulae, casks and commands"
    );
}

#[test]
fn to_sentence_joins_like_brew() {
    let s = |v: &[&str]| to_sentence(&v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    assert_eq!(s(&[]), "");
    assert_eq!(s(&["a"]), "a");
    assert_eq!(s(&["a", "b"]), "a and b");
    assert_eq!(s(&["a", "b", "c"]), "a, b, and c");
}

#[test]
fn update_report_diffs_indexes_like_brew() {
    use super::super::api::IndexRow;
    let row = |name: &str, version: &str| IndexRow {
        name: name.into(),
        desc: format!("{name} desc"),
        version: version.into(),
        auto_updates: false,
        oldnames: Vec::new(),
    };
    let inst = |name: &str, version: &str| Installed {
        kind: Kind::Formula,
        name: name.into(),
        version: version.into(),
        versions: vec![version.into()],
    };
    let old = vec![row("gone", "1"), row("kept", "1"), row("old-inst", "1")];
    let new = vec![row("kept", "2"), row("fresh", "1"), row("old-inst", "1")];
    let installed = vec![inst("kept", "1"), inst("gone", "1")];
    let (added, deleted, outdated) = update_report(Some(&old), &new, &installed, true);
    assert_eq!(added, [("fresh".to_string(), "fresh desc".to_string())]);
    assert_eq!(deleted, ["gone"]);
    assert_eq!(outdated, ["kept"]);
    let (added, deleted, outdated) = update_report(None, &new, &installed, true);
    assert!(added.is_empty() && deleted.is_empty());
    assert_eq!(outdated, ["kept"]);
    let (added, deleted, _) = update_report(Some(&old), &new, &installed, false);
    assert!(added.is_empty());
    assert_eq!(deleted, ["gone"]);
    let mut auto = row("kept", "2");
    auto.auto_updates = true;
    let (_, _, outdated) = update_report(None, &[auto], &installed, true);
    assert!(outdated.is_empty());
}

#[test]
fn list_files_excludes_directories_and_ds_store() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("keg");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("README"), b"r").unwrap();
    std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
    std::fs::write(root.join("bin/tool"), b"t").unwrap();
    std::fs::write(root.join("bin/.DS_Store"), b"junk").unwrap();
    #[cfg(unix)]
    super::super::prefix::symlink_file(&root.join("bin"), &root.join("binlink")).unwrap();
    let mut got: Vec<String> = list_files(&root)
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    got.sort();
    #[cfg(unix)]
    assert_eq!(got, ["README", "bin/tool", "binlink"]);
    #[cfg(not(unix))]
    assert_eq!(
        got,
        [
            "README".to_owned(),
            std::path::Path::new("bin")
                .join("tool")
                .display()
                .to_string()
        ]
    );
}

#[test]
fn artifact_lines_use_brew_summarize_and_english_names() {
    use super::super::formula::Artifact;
    assert_eq!(
        artifact_line(&Artifact::App("AltTab.app".into(), None)).as_deref(),
        Some("AltTab.app (App)")
    );
    assert_eq!(
        artifact_line(&Artifact::App("Foo.app".into(), Some("Bar.app".into()))).as_deref(),
        Some("Foo.app -> Bar.app (App)")
    );
    assert_eq!(
        artifact_line(&Artifact::Suite("SuiteDir".into(), None)).as_deref(),
        Some("SuiteDir (App Suite)")
    );
    assert_eq!(
        artifact_line(&Artifact::PrefPane("X.prefPane".into(), None)).as_deref(),
        Some("X.prefPane (Preference Pane)")
    );
    assert_eq!(
        artifact_line(&Artifact::Generic("f".into(), Some("/Library/X".into()))).as_deref(),
        Some("f -> /Library/X (Generic Artifact)")
    );
    assert_eq!(
        artifact_line(&Artifact::Wrapper(
            "demo".into(),
            "exe".into(),
            vec![],
            None
        ))
        .as_deref(),
        Some(".homebrew-command-wrappers/demo -> demo (Command Wrapper)")
    );
    assert_eq!(
        artifact_line(&Artifact::Library(
            "P.qlgenerator".into(),
            None,
            "Library/QuickLook".into()
        ))
        .as_deref(),
        Some("P.qlgenerator (Quick Look Plugin)")
    );
    assert_eq!(artifact_line(&Artifact::Other("zap".into())), None);
}

#[test]
fn shell_kind_follows_brew_families() {
    assert_eq!(shell_kind("fish"), ShellKind::Fish);
    assert_eq!(shell_kind("-fish"), ShellKind::Fish);
    assert_eq!(shell_kind("/usr/local/bin/fish"), ShellKind::Fish);
    assert_eq!(shell_kind("tcsh"), ShellKind::Csh);
    assert_eq!(shell_kind("-csh"), ShellKind::Csh);
    assert_eq!(shell_kind("pwsh"), ShellKind::Pwsh);
    assert_eq!(shell_kind("pwsh-preview"), ShellKind::Pwsh);
    assert_eq!(shell_kind("powershell"), ShellKind::Pwsh);
    assert_eq!(shell_kind("zsh"), ShellKind::Posix);
    assert_eq!(shell_kind("bash"), ShellKind::Posix);
    assert_eq!(shell_kind("nushell"), ShellKind::Posix);
    assert_eq!(shell_kind(""), ShellKind::Posix);
}

#[cfg(unix)]
#[test]
fn shellenv_keeps_prefix_bytes_literal_when_evaluated() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let sentinel = temp.path().join("unexpected");
    let root = temp.path().join(format!(
        "quote'\"$(touch {})`touch {}` path",
        sentinel.display(),
        sentinel.display()
    ));
    let prefix = Prefix::at(&root);
    let script = format!("{}\nprintf '%s' \"$ABREW_PREFIX\"", shellenv(&prefix));
    let output = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", &script])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, root.as_os_str().as_encoded_bytes());
    assert!(!sentinel.exists());
}

#[test]
fn deletion_requires_complete_dependency_and_request_inputs() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    for (name, on_request) in [("app", "true"), ("dep", "false"), ("orphan", "false")] {
        let keg = p.keg(name, "1");
        std::fs::create_dir_all(&keg).unwrap();
        std::fs::write(keg.join("payload"), name).unwrap();
        p.write_receipt(
            &keg,
            &[
                ("installed_by", "abrew"),
                ("name", name),
                ("version", "1"),
                ("installed_on_request", on_request),
            ],
        )
        .unwrap();
    }
    let read = |name: &str| {
        let deps = if name == "app" { vec!["dep"] } else { vec![] };
        super::super::api::dependency_formula(
            &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 0, "dependencies": deps}),
            name,
            super::super::machine::Machine::this(),
        )
    };
    assert_eq!(
        autoremove_targets(&p, read, &|_| None).unwrap(),
        vec!["orphan"]
    );
    let incomplete = |name: &str| {
        if name == "app" {
            Err("captured unknown".into())
        } else {
            read(name)
        }
    };
    assert!(autoremove_targets(&p, incomplete, &|_| None).is_err());
    for value in [
        aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0}),
        aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": "dep"}),
        aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": [1]}),
        aterm_json::json!({"name": "wrong", "versions": {"stable": "1"}, "revision": 0, "dependencies": []}),
        aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": ["../dep"]}),
        aterm_json::json!({"name": "app", "versions": {"stable": ""}, "revision": 0, "dependencies": []}),
        aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": "unknown", "dependencies": []}),
    ] {
        assert!(
            super::super::api::dependency_formula(
                &value,
                "app",
                super::super::machine::Machine::this()
            )
            .is_err()
        );
    }
    // (The revision-coverage gate this block used to assert
    // lived in install::dependency_books, removed in r370:
    // dependents now read receipt tab closures, exact per
    // keg, so metadata revision skew proves nothing either
    // way.)
    let receipt = p.keg("app", "1").join("INSTALL_RECEIPT.json");
    let saved = std::fs::read(&receipt).unwrap();
    std::fs::remove_file(&receipt).unwrap();
    assert!(autoremove_targets(&p, read, &|_| None).is_err());
    std::fs::write(
        &receipt,
        br#"{"installed_by":"abrew","name":"app","version":"1","installed_on_request":null}"#,
    )
    .unwrap();
    assert_eq!(
        autoremove_targets(&p, read, &|_| None).unwrap(),
        vec!["orphan"],
        "an absent request word never qualifies"
    );
    std::fs::write(&receipt, vec![b' '; MAX_REQUEST_RECEIPT_BYTES + 1]).unwrap();
    assert!(autoremove_targets(&p, read, &|_| None).is_err());
    std::fs::write(&receipt, saved).unwrap();
    let old = p.keg("orphan", "0");
    std::fs::create_dir_all(&old).unwrap();
    p.write_receipt(
        &old,
        &[
            ("installed_by", "abrew"),
            ("name", "orphan"),
            ("version", "0"),
            ("installed_on_request", "true"),
        ],
    )
    .unwrap();
    let older = p
        .installed_checked(Kind::Formula)
        .unwrap()
        .into_iter()
        .find(|i| i.name == "orphan")
        .unwrap();
    assert!(requested(&p, &older).unwrap(), "old request is retained");
    assert_eq!(
        autoremove_targets(&p, read, &|_| None).unwrap(),
        vec!["orphan"],
        "the latest keg's receipt decides, as brew's tab does"
    );
    std::fs::remove_dir_all(&old).unwrap();
    let missing = |name: &str| {
        super::super::api::dependency_formula(
            &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 0, "dependencies": ["absent"]}),
            name,
            super::super::machine::Machine::this(),
        )
    };
    assert_eq!(
        autoremove_targets(&p, missing, &|_| None).unwrap(),
        vec!["dep", "orphan"],
        "a dep name nothing installed answers to keeps nothing"
    );
    #[cfg(unix)]
    {
        p.pin("orphan").unwrap();
        assert!(autoremove_targets(&p, read, &|_| None).unwrap().is_empty());
        std::fs::remove_file(p.root.join("Pinned/orphan")).unwrap();
        std::fs::remove_dir(p.root.join("Pinned")).unwrap();
        std::fs::write(p.root.join("Pinned"), b"not a directory").unwrap();
        assert!(autoremove_targets(&p, read, &|_| None).is_err());
        std::fs::remove_file(p.root.join("Pinned")).unwrap();
        let pins_outside = tmp.path().join("pins-outside");
        std::fs::create_dir(&pins_outside).unwrap();
        std::os::unix::fs::symlink(&pins_outside, p.root.join("Pinned")).unwrap();
        assert!(autoremove_targets(&p, read, &|_| None).is_err());
        assert!(std::fs::read_dir(&pins_outside).unwrap().next().is_none());
        std::fs::remove_file(p.root.join("Pinned")).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("marker"), b"retain outside").unwrap();
        std::os::unix::fs::symlink(&outside, p.cellar().join("alias")).unwrap();
        assert!(p.installed_checked(Kind::Formula).is_err());
        assert_eq!(
            p.installed(Kind::Formula).len(),
            3,
            "legacy read-only best effort"
        );
        assert!(autoremove_targets(&p, read, &|_| None).is_err());
        assert_eq!(
            std::fs::read(outside.join("marker")).unwrap(),
            b"retain outside"
        );
        std::fs::remove_file(p.cellar().join("alias")).unwrap();
    }
    assert_eq!(
        autoremove_targets(&p, read, &|_| None).unwrap(),
        vec!["orphan"]
    );
    for name in ["adep", "zapp"] {
        let keg = p.keg(name, "1");
        std::fs::create_dir_all(&keg).unwrap();
        std::fs::write(keg.join("payload"), name).unwrap();
        p.write_receipt(
            &keg,
            &[
                ("installed_by", "abrew"),
                ("name", name),
                ("version", "1"),
                ("installed_on_request", "false"),
            ],
        )
        .unwrap();
    }
    let chain = |name: &str| {
        if name == "zapp" {
            super::super::api::dependency_formula(
                &aterm_json::json!({"name":name,"versions":{"stable":"1"},"revision":0,"dependencies":["adep"]}),
                name,
                super::super::machine::Machine::this(),
            )
        } else {
            read(name)
        }
    };
    assert_eq!(
        autoremove_targets(&p, chain, &|_| None).unwrap(),
        ["orphan", "zapp", "adep"]
    );
    let cycle = |name: &str| {
        if name == "adep" {
            super::super::api::dependency_formula(
                &aterm_json::json!({"name":name,"versions":{"stable":"1"},"revision":0,"dependencies":["zapp"]}),
                name,
                super::super::machine::Machine::this(),
            )
        } else {
            chain(name)
        }
    };
    assert_eq!(
        autoremove_targets(&p, cycle, &|_| None).unwrap(),
        vec!["orphan"],
        "cyclic orphans keep each other"
    );
    let unknown = p.keg("notes", "1");
    std::fs::create_dir_all(&unknown).unwrap();
    std::fs::write(unknown.join("owner"), b"keep").unwrap();
    assert!(autoremove_targets(&p, chain, &|_| None).is_err());
    assert_eq!(std::fs::read(unknown.join("owner")).unwrap(), b"keep");
    for name in ["app", "dep", "orphan", "adep", "zapp"] {
        assert_eq!(
            std::fs::read(p.keg(name, "1").join("payload")).unwrap(),
            name.as_bytes()
        );
    }
}

#[test]
fn autoremove_reads_recorded_closures_and_keeps_cask_needs() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    // app's receipt records dep as its runtime closure,
    // while the current definitions have moved on (app no
    // longer declares dep, and gone has no definition).
    let rows: [(&str, &str, Option<&str>); 4] = [
        ("app", "true", Some("[{\"full_name\": \"dep\"}]")),
        ("dep", "false", Some("[]")),
        ("gone", "false", Some("[]")),
        ("caskdep", "false", Some("[]")),
    ];
    for (name, on_request, tab) in rows {
        let keg = p.keg(name, "1");
        std::fs::create_dir_all(&keg).unwrap();
        let fields: Vec<(&str, &str)> = vec![
            ("installed_by", "abrew"),
            ("name", name),
            ("version", "1"),
            ("installed_on_request", on_request),
        ];
        match tab {
            Some(tab) => p
                .write_receipt_raw(&keg, &fields, &[("runtime_dependencies", tab)])
                .unwrap(),
            None => p.write_receipt(&keg, &fields).unwrap(),
        }
    }
    let read = |name: &str| {
        if name == "gone" {
            return Err("captured unknown".into());
        }
        super::super::api::dependency_formula(
            &aterm_json::json!({"name": name, "versions": {"stable": "2"}, "revision": 0, "dependencies": []}),
            name,
            super::super::machine::Machine::this(),
        )
    };
    assert_eq!(
        autoremove_targets(&p, read, &|_| None).unwrap(),
        vec!["caskdep", "gone"],
        "the recorded closure keeps dep and needs no definition for gone"
    );
    // A cask depending on caskdep protects it.
    let payload = p.cask_dir("tool", "1");
    std::fs::create_dir_all(&payload).unwrap();
    p.write_receipt(
        &payload,
        &[
            ("installed_by", "abrew"),
            ("token", "tool"),
            ("version", "1"),
            ("installed_on_request", "true"),
        ],
    )
    .unwrap();
    assert_eq!(
        autoremove_targets(&p, read, &|token| {
            (token == "tool").then(|| vec!["caskdep".to_string()])
        })
        .unwrap(),
        vec!["gone"]
    );
}

#[test]
fn caveats_helpers_match_brew_text() {
    // KegOnlyReason#to_s: explanation wins; stock symbol texts
    // are brew's verbatim (wrapped) wordings.
    assert_eq!(
        keg_only_reason_text(
            ":shadowed_by_macos",
            "macOS provides BLAS in Accelerate.framework"
        ),
        "macOS provides BLAS in Accelerate.framework"
    );
    assert_eq!(
        keg_only_reason_text(":provided_by_macos", ""),
        "macOS already provides this software and installing another version in\nparallel can cause all kinds of trouble"
    );
    assert_eq!(
        keg_only_reason_text(":versioned_formula", ""),
        "this is an alternate version of another formula"
    );
    assert_eq!(
        keg_only_reason_text("it conflicts with x", ""),
        "it conflicts with x"
    );
    // Shellwords.escape via Utils::Shell.sh_quote.
    assert_eq!(sh_quote(""), "''");
    assert_eq!(sh_quote("/opt/x/bin"), "/opt/x/bin");
    assert_eq!(sh_quote("~/.zshrc"), "~/.zshrc");
    assert_eq!(sh_quote("a b=c$d"), "a\\ b\\=c\\$d");
    // export_value per shell.
    assert_eq!(
        export_value(Some("zsh"), "LDFLAGS", "-L/opt/x/lib").as_deref(),
        Some("export LDFLAGS=-L/opt/x/lib")
    );
    assert_eq!(
        export_value(Some("fish"), "LDFLAGS", "-L/opt/x/lib").as_deref(),
        Some("set -gx LDFLAGS -L/opt/x/lib")
    );
    assert_eq!(
        export_value(Some("tcsh"), "LDFLAGS", "-L/opt/x/lib").as_deref(),
        Some("setenv LDFLAGS -L/opt/x/lib;")
    );
    assert_eq!(export_value(None, "LDFLAGS", "-L/opt/x/lib"), None);
    assert_eq!(
        prepend_path_in_profile(Some("fish"), "/opt/x/bin").as_deref(),
        Some("fish_add_path /opt/x/bin")
    );
    let zsh = prepend_path_in_profile(Some("zsh"), "/opt/x/bin").unwrap();
    assert!(
        zsh.starts_with("printf '%s\\n' export\\ PATH\\=/opt/x/bin:\\$PATH >> "),
        "{zsh}"
    );
    // Service#manual_command: env (minus PATH), placeholder
    // expansion, shell-quoted argv.
    let service = super::super::formula::ServiceInfo {
        run: vec![
            "$HOMEBREW_PREFIX/opt/apfel/bin/apfel".to_string(),
            "--serve".to_string(),
        ],
        require_root: false,
        environment_variables: vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("FOO".to_string(), "$HOMEBREW_PREFIX/var".to_string()),
        ],
        name: None,
        working_dir: None,
        root_dir: None,
        log_path: None,
        error_log_path: None,
        interval: None,
        cron: None,
    };
    assert_eq!(
        service_manual_command("/p", &service),
        "FOO=\"/p/var\" /p/opt/apfel/bin/apfel --serve"
    );
}

#[test]
fn installed_section_includes_sibling_kegs() {
    let dir = crate::testdir::dir("abrew-info-siblings-");
    let p = Prefix::at(&dir.join("prefix"));
    p.ensure().unwrap();
    owned_keg(&p, "demo", "1.0");
    owned_keg(&p, "demo", "2.0");
    owned_keg(&p, "demo@1", "1.5");
    owned_keg(&p, "unrelated", "9.9");
    p.ensure_api_cache().unwrap();
    std::fs::write(
        p.cache().join("api/formula-demo@1.json"),
        br#"{"name":"demo@1","full_name":"demo@1","versions":{"stable":"1.5"}}"#,
    )
    .unwrap();
    let doc =
        aterm_json::from_str::<aterm_json::Value>(r#"{"name":"demo","versions":{"stable":"2.0"}}"#)
            .unwrap();
    let f = super::super::formula::Formula::from_api(&doc).unwrap();
    let inst = p.installed_one(Kind::Formula, "demo").unwrap();
    let api = Api::read_only(p.clone());
    // Non-verbose: newest keg per related formula, newest first.
    let rows = installed_section_rows(&p, &api, &f, Some(&inst), false);
    let names: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.full_name.as_str(), r.shown.as_str()))
        .collect();
    assert_eq!(names, [("demo", "2.0"), ("demo@1", "1.5")]);
    // Verbose: every keg.
    let rows = installed_section_rows(&p, &api, &f, Some(&inst), true);
    let names: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.full_name.as_str(), r.shown.as_str()))
        .collect();
    assert_eq!(names, [("demo", "2.0"), ("demo", "1.0"), ("demo@1", "1.5")]);
    // Not installed itself: the sibling's kegs still show.
    let rows = installed_section_rows(&p, &api, &f, None, false);
    let names: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.full_name.as_str(), r.shown.as_str()))
        .collect();
    assert_eq!(names, [("demo@1", "1.5")]);
    // A higher receipt version_scheme outranks a higher version
    // (brew's openssl@3 [scheme 1] above openssl@4 [scheme 0]).
    p.write_receipt(
        &p.keg("demo@1", "1.5"),
        &[
            ("installed_by", "abrew"),
            ("name", "demo@1"),
            ("version", "1.5"),
            ("version_scheme", "2"),
        ],
    )
    .unwrap();
    let rows = installed_section_rows(&p, &api, &f, Some(&inst), false);
    let names: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.full_name.as_str(), r.shown.as_str()))
        .collect();
    assert_eq!(names, [("demo@1", "1.5"), ("demo", "2.0")]);
}

/// Unix only: abrew runs nothing on Windows (`run` refuses there), where
/// `same_path_identity` answers no and so every removal refuses, failing safe.
#[cfg(unix)]
#[test]
fn caffiene_alias_works_for_list_outdated_and_uninstall() {
    let dir = crate::testdir::dir("abrew-caffiene-lifecycle-");
    let p = Prefix::at(&dir.join("prefix"));
    p.ensure().unwrap();
    let cask_dir = p.cask_dir("caffeine", "1.1.3");
    std::fs::create_dir_all(&cask_dir).unwrap();
    std::fs::write(cask_dir.join("Caffeine.app"), b"stub").unwrap();
    p.write_receipt(
        &cask_dir,
        &[
            ("installed_by", "abrew"),
            ("token", "caffeine"),
            ("version", "1.1.3"),
            ("installed_on_request", "true"),
        ],
    )
    .unwrap();
    p.rebuild_state().unwrap();

    let api = Api::read_only(p.clone());
    assert_eq!(
        cmd_list(&p, &api, &[String::from("caffiene")]),
        ExitCode::SUCCESS
    );
    assert_eq!(
        cmd_list(
            &p,
            &api,
            &[String::from("--versions"), String::from("caffiene")]
        ),
        ExitCode::SUCCESS
    );
    assert!(!package_definitively_unknown(
        &p, &api, "caffiene", false, true
    ));
    assert_eq!(
        cmd_unpin(&p, &api, &[String::from("caffiene")]),
        ExitCode::SUCCESS
    );
    assert_eq!(
        bundle_check(
            &p,
            &api,
            &[String::from("check"), String::from("--no-upgrade")],
            &[bundle::BrewfileEntry::Cask(String::from("caffiene"))],
        ),
        ExitCode::SUCCESS
    );
    assert_eq!(
        cmd_cleanup(&p, &api, &[String::from("caffiene")]),
        ExitCode::SUCCESS
    );
    assert_eq!(
        cmd_uninstall(
            &p,
            &api,
            &[String::from("uninstall"), String::from("caffiene")],
            &[String::from("caffiene")]
        ),
        ExitCode::SUCCESS
    );
    assert!(p.installed_one(Kind::Cask, "caffeine").is_none());
}

#[test]
fn bundle_check_missing_entries_fail_without_api() {
    let dir = crate::testdir::dir("abrew-bundle-check-");
    let p = Prefix::at(&dir.join("prefix"));
    p.ensure().unwrap();
    let api = Api::read_only(p.clone());
    let args = [String::from("check")];
    assert_eq!(
        bundle_check(
            &p,
            &api,
            &args,
            &[
                bundle::BrewfileEntry::Tap(String::from("someone/tools")),
                bundle::BrewfileEntry::Cask(String::from("no-such-cask-xyz")),
                bundle::BrewfileEntry::Brew(String::from("no-such-formula-xyz")),
            ],
        ),
        ExitCode::FAILURE
    );
    assert_eq!(bundle_check(&p, &api, &args, &[]), ExitCode::SUCCESS);
}

/// r375: brew's bundle editors accept fully qualified and
/// `user/tap` names — the gate must use the same component
/// rule `brew edit`/`tap` use, not the plain-name one (the
/// live pair matrix covers resolution/trust/files).
#[test]
fn bundle_edit_gate_accepts_qualified_names() {
    for n in ["probeuser/probe/custool", "homebrew/core/wget", "wget"] {
        assert!(super::super::valid_name(n), "{n}");
    }
    for n in ["a/../x", "/abs", "a/b/c/d", "a//b"] {
        assert!(!super::super::valid_name(n), "{n}");
    }
    let names = vec!["probeuser/probe/custool".to_owned()];
    assert_eq!(
        bundle_remove_match("brew \"probeuser/probe/custool\"", None, &names).as_deref(),
        Some("probeuser/probe/custool")
    );
}

#[test]
fn bundle_remove_match_follows_brew_regex_shape() {
    let names = vec!["wget".to_string()];
    let m = |line: &str, kind: Option<&str>| bundle_remove_match(line, kind, &names);
    assert_eq!(m("brew \"wget\"", None).as_deref(), Some("wget"));
    assert_eq!(m("brew \"wget\"", Some("brew")).as_deref(), Some("wget"));
    assert_eq!(m("brew \"wget\"", Some("cask")), None);
    assert_eq!(m("brew(\"wget\")", Some("brew")).as_deref(), Some("wget"));
    assert_eq!(m("brew 'wget'", None), None);
    assert_eq!(m("# brew \"wget\"", None).as_deref(), Some("wget"));
    assert_eq!(m("brew \"wget2\"", None), None);
    assert_eq!(m("cask \"wget\"", Some("brew")), None);
    assert_eq!(
        m("brew \"wget\", link: false", None).as_deref(),
        Some("wget")
    );
}

#[cfg(unix)]
#[test]
fn pinned_metadata_line_reads_cask_pin_timestamp_from_pinned_casks() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let cask_dir = p.cask_dir("demo", "2.4");
    std::fs::create_dir_all(&cask_dir).unwrap();
    p.write_receipt(
        &cask_dir,
        &[
            ("installed_by", "abrew"),
            ("token", "demo"),
            ("version", "2.4"),
        ],
    )
    .unwrap();
    p.pin_cask("demo").unwrap();
    let line = pinned_metadata_line(&p, "demo", true).expect("pinned cask metadata");
    assert!(line.starts_with("Pinned: 2.4 on "), "{line}");
    assert!(pinned_metadata_line(&p, "demo", false).is_none());
}

#[cfg(unix)]
#[test]
fn cask_and_formula_pins_govern_cleanup_unpin_and_uninstall() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let api = Api::read_only(p.clone());

    // Cask pinned at 1.0 with 2.0 also installed: cleanup_old_versions keeps 1.0.
    for v in ["1.0", "2.0"] {
        let dir = p.cask_dir("demo", v);
        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[("installed_by", "abrew"), ("token", "demo"), ("version", v)],
        )
        .unwrap();
    }
    std::fs::create_dir_all(p.root.join("var/homebrew/pinned_casks")).unwrap();
    super::super::prefix::symlink_dir(
        std::path::Path::new("../../../Caskroom/demo/1.0"),
        &p.root.join("var/homebrew/pinned_casks/demo"),
    )
    .unwrap();
    assert_eq!(p.cask_pinned_version("demo").as_deref(), Some("1.0"));
    assert_eq!(
        cleanup_old_versions(&p, &api, false, true, &[], &mut Vec::new())
            .unwrap()
            .0,
        0
    );
    assert!(p.cask_dir("demo", "1.0").is_dir());
    assert!(p.cask_dir("demo", "2.0").is_dir());

    // Both formula and cask `demo` pinned: `unpin --cask` unpins only the cask.
    owned_keg(&p, "demo", "1.0");
    p.pin("demo").unwrap();
    assert!(p.is_pinned("demo"));
    assert!(p.cask_is_pinned("demo"));
    assert_eq!(
        cmd_unpin(&p, &api, &[String::from("--cask"), String::from("demo")]),
        ExitCode::SUCCESS
    );
    assert!(p.is_pinned("demo"));
    assert!(!p.cask_is_pinned("demo"));
    assert!(!p.cask_pin_link_present("demo"));

    // Re-pin cask `demo`: dry-run uninstall without --force refuses, and --force uninstall
    // removes the cask pin link along with the caskroom payloads.
    p.pin_cask("demo").unwrap();
    assert_eq!(
        cmd_uninstall(
            &p,
            &api,
            &[
                String::from("uninstall"),
                String::from("--cask"),
                String::from("--dry-run"),
                String::from("demo"),
            ],
            &[
                String::from("--cask"),
                String::from("--dry-run"),
                String::from("demo"),
            ],
        ),
        ExitCode::FAILURE
    );
    assert_eq!(
        cmd_uninstall(
            &p,
            &api,
            &[
                String::from("uninstall"),
                String::from("--cask"),
                String::from("--force"),
                String::from("demo"),
            ],
            &[
                String::from("--cask"),
                String::from("--force"),
                String::from("demo"),
            ],
        ),
        ExitCode::SUCCESS
    );
    assert!(p.installed_one(Kind::Cask, "demo").is_none());
    assert!(!p.cask_pin_link_present("demo"));

    // Formula with a dangling owned pin link cleans up the dangling pin when its last keg is uninstalled.
    p.unpin("demo").unwrap();
    super::super::prefix::symlink_dir(&p.keg("demo", "0.9"), &p.root.join("Pinned/demo")).unwrap();
    assert!(p.pin_link_dangling_ours("demo"));
    install::uninstall(&api, &p, "demo", true, false, false).unwrap();
    assert!(p.installed_one(Kind::Formula, "demo").is_none());
    assert!(!p.pin_link_dangling_ours("demo"));
    assert!(std::fs::symlink_metadata(p.root.join("Pinned/demo")).is_err());
}

#[test]
fn migration_merge_keeps_new_copies_and_moves_old_versions() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    for (root, ver, marker) in [
        (&old, "1.0", "old1"),
        (&old, "2.0", "old2"),
        (&new, "2.0", "new2"),
    ] {
        let bin = root.join(ver).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("tool"), marker).unwrap();
    }
    assert!(!remove_migration_conflicts(&old, &old, &new).unwrap());
    merge_cellar_directory(&old, &old, &new).unwrap();
    std::fs::remove_dir(&old).unwrap();
    assert_eq!(
        std::fs::read_to_string(new.join("1.0/bin/tool")).unwrap(),
        "old1"
    );
    assert_eq!(
        std::fs::read_to_string(new.join("2.0/bin/tool")).unwrap(),
        "new2"
    );
}

#[cfg(unix)]
#[test]
fn caskroom_copy_recreates_symlinks() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let src = tmp.path().join("src/1.0");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("real"), b"payload").unwrap();
    std::os::unix::fs::symlink("real", src.join("alias")).unwrap();
    let dst = tmp.path().join("dst");
    copy_dir_preserving_links(&tmp.path().join("src"), &dst).unwrap();
    let alias = dst.join("1.0/alias");
    assert!(alias.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&alias).unwrap(), "payload");
}

#[cfg(unix)]
#[test]
fn completions_link_state_unlink_round_trip() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let tap = p.taps_dir().join("someone/homebrew-tools/completions/zsh");
    std::fs::create_dir_all(&tap).unwrap();
    std::fs::create_dir_all(p.taps_dir().join("someone/homebrew-tools/.git")).unwrap();
    std::fs::write(tap.join("_tool"), b"#compdef tool").unwrap();
    let rest = |s: &str| vec![s.to_string()];
    assert_eq!(cmd_completions(&p, &[]), ExitCode::SUCCESS);
    assert!(!tap::completions_linked(&p));
    assert_eq!(cmd_completions(&p, &rest("link")), ExitCode::SUCCESS);
    assert!(tap::completions_linked(&p));
    let linked = p.root.join("share/zsh/site-functions/_tool");
    assert!(linked.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::canonicalize(&linked).unwrap(),
        std::fs::canonicalize(tap.join("_tool")).unwrap()
    );
    assert_eq!(cmd_completions(&p, &rest("unlink")), ExitCode::SUCCESS);
    assert!(!tap::completions_linked(&p));
    assert!(linked.symlink_metadata().is_err());
}

#[test]
fn json_pretty_matches_ruby_layout() {
    let mut inner = aterm_json::Map::new();
    inner.insert("b".into(), aterm_json::Value::Array(vec![]));
    let mut root = aterm_json::Map::new();
    root.insert("a".into(), aterm_json::Value::Object(inner));
    root.insert(
        "c".into(),
        aterm_json::Value::Array(vec![
            aterm_json::Value::Number(1u64.into()),
            aterm_json::Value::String("x".into()),
        ]),
    );
    assert_eq!(
        jpretty(&JVal::Val(aterm_json::Value::Object(root))),
        "{\n  \"a\": {\n    \"b\": []\n  },\n  \"c\": [\n    1,\n    \"x\"\n  ]\n}"
    );
    // Ordered objects keep insertion order in both layouts.
    let ordered = JVal::Obj(vec![
        ("formulae".into(), JVal::Arr(vec![])),
        ("casks".into(), JVal::Arr(vec![])),
    ]);
    assert_eq!(
        jpretty(&ordered),
        "{\n  \"formulae\": [],\n  \"casks\": []\n}"
    );
    assert_eq!(jcompact(&ordered), "{\"formulae\":[],\"casks\":[]}");
}

#[test]
fn ordered_emission_follows_source_key_order() {
    let source = br#"{"name":"jq","versions":{"stable":"1.8"},"installed":[],"analytics":{}}"#;
    let guide = parse_order(source).expect("guide");
    let value: aterm_json::Value = aterm_json::from_slice(
        br#"{"analytics":1,"installed":[{"version":"1.7","sbom":null,"time":5}],"name":"jq","versions":{"stable":"1.8"},"zz_new":true}"#,
    )
    .unwrap();
    let mut out = String::new();
    write_ordered(&value, &guide, 0, &mut out);
    // Source order first (guide-less keys last); the installed
    // row takes brew's Tab order (sbom before version).
    assert_eq!(
        out,
        "{\n  \"name\": \"jq\",\n  \"versions\": {\n    \"stable\": \"1.8\"\n  },\n  \"installed\": [\n    {\n      \"sbom\": null,\n      \"version\": \"1.7\",\n      \"time\": 5\n    }\n  ],\n  \"analytics\": 1,\n  \"zz_new\": true\n}"
    );
    assert!(parse_order(b"{\"a\":").is_none());
    assert!(parse_order(br#"["x", {"k": null}]"#).is_some());
}
