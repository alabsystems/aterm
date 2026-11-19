// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
fn git_ok(args: &[&str]) {
    let out = fetch::run_capture(aterm_uds::os_path!("git"), args).unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// r376: livecheck blocks parse as data across the shapes
/// the core tap actually uses (samples verbatim from
/// homebrew-core); interpolation and unknown statements
/// refuse the whole block (fail-closed).
#[test]
fn livecheck_block_parses_as_data() {
    use super::super::formula::LivecheckUrl;
    let spec = parse_livecheck_block(
        "class Jq < Formula\n  livecheck do\n    url :stable\n    regex(/^(?:jq[._-])?v?(\\d+(?:\\.\\d+)+)$/i)\n  end\nend\n",
    )
    .unwrap();
    assert_eq!(spec.url, Some(LivecheckUrl::Symbol("stable".into())));
    assert_eq!(
        spec.regex.as_deref(),
        Some(r"^(?:jq[._-])?v?(\d+(?:\.\d+)+)$")
    );
    assert_eq!(spec.regex_flags, "i");
    assert!(!spec.url_options && !spec.strategy_block);

    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://files.radicle.xyz/releases/latest/radicle.json\"\n    strategy :json do |json|\n      json[\"version\"]\n    end\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.url,
        Some(LivecheckUrl::Literal(
            "https://files.radicle.xyz/releases/latest/radicle.json".into()
        ))
    );
    assert_eq!(spec.strategy.as_deref(), Some("json"));
    assert!(spec.strategy_block);

    let spec = parse_livecheck_block(
        "  livecheck do\n    regex(%r{<version>v?(\\d+(?:\\.\\d+)+)</version>}i)\n    throttle days: 1\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.regex.as_deref(),
        Some(r"<version>v?(\d+(?:\.\d+)+)</version>")
    );
    assert_eq!(spec.throttle_days, Some(1));

    let spec = parse_livecheck_block(
        "  livecheck do\n    skip \"Not actively developed or maintained\"\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.skip.as_deref(),
        Some("Not actively developed or maintained")
    );

    // Nested resource block is not the package block, and a
    // block after it still resolves as the package one.
    let none = parse_livecheck_block(
        "  resource \"man\" do\n    livecheck do\n      formula :parent\n    end\n  end\n",
    );
    assert!(none.is_none());
    let spec = parse_livecheck_block(
        "  livecheck do\n    formula \"git\"\n  end\n  resource \"man\" do\n    livecheck do\n      formula :parent\n    end\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.formula_ref.as_deref(), Some("git"));

    // Interpolation and unknown statements refuse, except
    // the version forms a url may carry (r386): stored
    // verbatim for `#{version...}`, rewritten for
    // `#{LATEST_VERSION}`.
    assert!(
        parse_livecheck_block("  livecheck do\n    url \"https://example.com/#{foo}/\"\n  end\n")
            .is_none()
    );
    // r444: version chains in a regex stay verbatim
    // for plan-time substitution (bartender).
    let spec = parse_livecheck_block("  livecheck do\n    regex(/v#{version}/)\n  end\n").unwrap();
    assert_eq!(spec.regex.as_deref(), Some("v#{version}"));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/channels/#{version.major}/v#{LATEST_VERSION}/x\"\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.url,
        Some(super::super::formula::LivecheckUrl::Literal(
            "https://example.com/channels/#{version.major}/v<FORMULA_LATEST_VERSION>/x".into()
        ))
    );
    assert!(
        parse_livecheck_block("  livecheck do\n    url :stable\n    whatever 1\n  end\n").is_none()
    );
    assert!(parse_livecheck_block("class X < Formula\nend\n").is_none());
}

#[test]
fn tap_add_refusals_match_brew() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let err = |name: &str, url: Option<&str>, custom: bool, force: bool| {
        add(&prefix, name, url, custom, force).unwrap_err()
    };
    assert!(err("homebrew/services", None, false, false).contains("was deprecated"));
    assert!(err("homebrew/bundle", None, false, false).contains("was deprecated"));
    assert_eq!(
        err("caskroom/fonts", None, false, false),
        "Error: caskroom/fonts was moved. Tap homebrew/cask-fonts instead."
    );
    assert_eq!(
        err("someone/tap", None, true, false),
        "Error: Tap someone/tap with option `--custom-remote` but without a remote URL."
    );
    assert!(err("homebrew/core", None, false, false).contains("--force"));
    assert!(err("homebrew/cask", None, false, false).contains("--force"));
    assert!(err("linuxbrew/core", None, false, false).contains("--force"));
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn tap_add_mismatch_and_custom_remote() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let origin_s = origin.to_string_lossy().into_owned();
    git_ok(&["-C", &origin_s, "init", "-q", "-b", "main"]);
    std::fs::write(origin.join("f.rb"), b"# tap").unwrap();
    git_ok(&["-C", &origin_s, "add", "f.rb"]);
    git_ok(&[
        "-C",
        &origin_s,
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "one",
    ]);
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let (tap, newly) = add(&prefix, "someone/tap", Some(&origin_s), false, false).unwrap();
    assert!(newly);
    // Re-tap with a different URL refuses with brew's mismatch text.
    let error = add(&prefix, "someone/tap", Some("/elsewhere"), false, false).unwrap_err();
    assert_eq!(
        error,
        format!("Error: Tap someone/tap remote mismatch.\n{origin_s} != /elsewhere")
    );
    // --custom-remote repoints the tap at the new remote.
    let mirror = tmp.path().join("mirror");
    let mirror_s = mirror.to_string_lossy().into_owned();
    git_ok(&["clone", "-q", &origin_s, &mirror_s]);
    add(&prefix, "someone/tap", Some(&mirror_s), true, false).unwrap();
    let remote = git_out(&tap.path, &["remote", "get-url", "origin"]).unwrap();
    assert_eq!(remote, mirror_s);
}

// abrew refuses to run on Windows (Homebrew is macOS/Linux software).
#[cfg(unix)]
#[test]
fn repair_follows_an_upstream_default_branch_rename() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let origin_s = origin.to_string_lossy().into_owned();
    git_ok(&["-C", &origin_s, "init", "-q", "-b", "trunk"]);
    std::fs::write(origin.join("f.rb"), b"# tap").unwrap();
    git_ok(&["-C", &origin_s, "add", "f.rb"]);
    git_ok(&[
        "-C",
        &origin_s,
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "one",
    ]);
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let tap_dir = prefix.taps_dir().join("someone/homebrew-tap");
    std::fs::create_dir_all(tap_dir.parent().unwrap()).unwrap();
    let tap_s = tap_dir.to_string_lossy().into_owned();
    git_ok(&["clone", "-q", &origin_s, &tap_s]);
    // Stale state: local branch and origin/HEAD still say master.
    git_ok(&["-C", &tap_s, "branch", "-m", "trunk", "master"]);
    git_ok(&[
        "-C",
        &tap_s,
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/master",
    ]);
    repair(&prefix).unwrap();
    let branches = git_out(&tap_dir, &["branch", "--format=%(refname:short)"]).unwrap();
    assert_eq!(branches, "trunk");
    assert_eq!(origin_branch_name(&tap_dir).as_deref(), Some("trunk"));
    // A second repair is a quiet no-op.
    repair(&prefix).unwrap();
    // No taps at all is fine too.
    let empty = Prefix::at(&tmp.path().join("empty"));
    empty.ensure().unwrap();
    repair(&empty).unwrap();
}

#[test]
fn untap_refuses_while_tap_packages_are_installed() {
    fn payload(p: &Prefix, kind: Kind, version: &str, full: Option<&str>) -> PathBuf {
        let dir = match kind {
            Kind::Formula => p.keg("tool", version),
            Kind::Cask => p.cask_dir("tool", version),
        };
        std::fs::create_dir_all(&dir).unwrap();
        let mut fields = vec![
            ("installed_by", "abrew"),
            (
                match kind {
                    Kind::Formula => "name",
                    Kind::Cask => "token",
                },
                "tool",
            ),
            ("version", version),
        ];
        if let Some(full) = full {
            fields.push(("full_name", full));
        }
        p.write_receipt(&dir, &fields).unwrap();
        dir
    }
    fn tapped(p: &Prefix) -> PathBuf {
        let tap = p.taps_dir().join("someone/homebrew-tap");
        std::fs::create_dir_all(&tap).unwrap();
        std::fs::write(tap.join("KEEP"), b"tap recipes").unwrap();
        tap
    }
    fn refused(p: &Prefix, tap: &std::path::Path) -> String {
        let error = remove(p, "someone/tap").unwrap_err();
        assert_eq!(std::fs::read(tap.join("KEEP")).unwrap(), b"tap recipes");
        error
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    for kind in [Kind::Formula, Kind::Cask] {
        let p = Prefix::at(&tmp.path().join(kind.word()));
        p.ensure().unwrap();
        let official = match kind {
            Kind::Formula => "homebrew/core/tool",
            Kind::Cask => "homebrew/cask/tool",
        };
        let old = payload(&p, kind, "1.0", Some("someone/tap/tool"));
        let current = payload(&p, kind, "2.0", Some(official));
        let tap = tapped(&p);
        let error = refused(&p, &tap);
        assert!(error.contains("Refusing to untap someone/tap"), "{error}");
        assert!(error.contains("someone/tap/tool"), "{error}");
        assert!(old.is_dir() && current.is_dir());
        payload(&p, kind, "1.0", Some("someone/homebrew-tap/tool"));
        assert!(refused(&p, &tap).contains("someone/tap/tool"));
        std::fs::remove_dir_all(&old).unwrap();
        let receipt = current.join("INSTALL_RECEIPT.json");
        let official_bytes = std::fs::read(&receipt).unwrap();
        remove(&p, "someone/tap").unwrap();
        assert!(
            !tap.exists(),
            "an unrelated installed version is no use of this tap"
        );
        assert_eq!(std::fs::read(&receipt).unwrap(), official_bytes);

        let tap = tapped(&p);
        let key = match kind {
            Kind::Formula => "name",
            Kind::Cask => "token",
        };
        for full in [
            "null",
            "42",
            "\"\"",
            "\"other\"",
            "\"someone/tap/other\"",
            "\"someone/tool\"",
            "\"../tap/tool\"",
        ] {
            let bytes = format!(
                "{{\"installed_by\":\"abrew\",\"{key}\":\"tool\",\"version\":\"2.0\",\"full_name\":{full}}}"
            );
            std::fs::write(&receipt, bytes.as_bytes()).unwrap();
            let _ = refused(&p, &tap);
            assert_eq!(std::fs::read(&receipt).unwrap(), bytes.as_bytes());
        }
        for bytes in [b"{".as_slice(), b"[]".as_slice()] {
            std::fs::write(&receipt, bytes).unwrap();
            let _ = refused(&p, &tap);
            assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
        }
        std::fs::remove_file(&receipt).unwrap();
        let _ = refused(&p, &tap);
        std::fs::create_dir(&receipt).unwrap();
        let _ = refused(&p, &tap);
        assert!(receipt.is_dir());
        std::fs::remove_dir(&receipt).unwrap();
        let oversized = vec![b' '; 64 * 1024 + 1];
        std::fs::write(&receipt, &oversized).unwrap();
        let _ = refused(&p, &tap);
        assert_eq!(
            std::fs::metadata(&receipt).unwrap().len(),
            oversized.len() as u64
        );
        std::fs::remove_file(&receipt).unwrap();

        #[cfg(unix)]
        {
            let external = tmp.path().join(format!("{}-receipt", kind.word()));
            std::fs::write(&external, &official_bytes).unwrap();
            std::os::unix::fs::symlink(&external, &receipt).unwrap();
            let _ = refused(&p, &tap);
            assert_eq!(std::fs::read_link(&receipt).unwrap(), external);
            assert_eq!(std::fs::read(&external).unwrap(), official_bytes);
            std::fs::remove_file(&receipt).unwrap();
            payload(&p, kind, "2.0", Some(official));
            for version_only in [false, true] {
                let row = if version_only {
                    current.clone()
                } else {
                    current.parent().unwrap().to_path_buf()
                };
                let held = tmp
                    .path()
                    .join(format!("{}-held-{version_only}", kind.word()));
                std::fs::rename(&row, &held).unwrap();
                std::os::unix::fs::symlink(&held, &row).unwrap();
                let _ = refused(&p, &tap);
                assert_eq!(std::fs::read_link(&row).unwrap(), held);
                std::fs::remove_file(&row).unwrap();
                std::fs::rename(&held, &row).unwrap();
            }
        }
        for full in [None, Some("tool")] {
            payload(&p, kind, "2.0", full);
            let tap = tapped(&p);
            remove(&p, "someone/tap").unwrap();
            assert!(!tap.exists());
            assert!(current.is_dir());
        }
    }
}

#[test]
fn local_remotes_are_the_only_file_transport() {
    for local in ["/tmp/tap", "./tap", "../tap", "~/tap", "file:///tmp/tap"] {
        assert!(is_local_remote(local), "{local}");
        assert!(
            git_protocol_args(true).contains(&"protocol.file.allow=always"),
            "{local}"
        );
    }
    for remote in [
        "https://github.com/me/homebrew-tools.git",
        "git@github.com:me/homebrew-tools.git",
        "ssh://git@example.com/x.git",
        "ext::sh -c true",
        "fd::0",
    ] {
        assert!(!is_local_remote(remote), "{remote}");
    }
    assert!(
        !git_protocol_args(false).contains(&"protocol.file.allow=always"),
        "non-local remotes never allow the file transport"
    );
}

#[test]
fn rb_parse_as_data() {
    let rb = "class Foo < Formula\n  desc \"A foo\"\n  homepage \"https://foo.example\"\n  url \"https://foo.example/foo-2.1.tar.gz\"\n  sha256 \"aaaa\"\n  depends_on \"openssl@3\"\nend\n";
    let f = parse_rb_as_data(rb, "me/tap/foo");
    assert_eq!(f.name, "foo");
    assert_eq!(f.version, "2.1");
    assert_eq!(f.stable_sha256, "aaaa");
    assert_eq!(f.dependencies, ["openssl@3"]);
}

#[test]
fn rb_parse_service_block_as_data() {
    let rb = "class Foo < Formula\n  url \"https://foo.example/foo-2.1.tar.gz\"\n  service do\n    run [opt_bin/\"foo\", \"--serve\", \"-c\", etc/\"foo.conf\"]\n    require_root true\n    environment_variables FOO: \"bar\"\n    log_path var/\"log/foo.log\"\n    error_log_path var/\"log/foo.err\"\n    interval 3600\n    name macos: \"com.example.foo\"\n  end\nend\n";
    let f = parse_rb_as_data(rb, "me/tap/foo");
    let svc = f.service.expect("service block parsed");
    assert_eq!(
        svc.run,
        [
            "$HOMEBREW_PREFIX/opt/foo/bin/foo",
            "--serve",
            "-c",
            "$HOMEBREW_PREFIX/etc/foo.conf",
        ]
    );
    assert!(svc.require_root);
    assert_eq!(
        svc.environment_variables,
        [("FOO".to_string(), "bar".to_string())]
    );
    assert_eq!(
        svc.log_path.as_deref(),
        Some("$HOMEBREW_PREFIX/var/log/foo.log")
    );
    assert_eq!(
        svc.error_log_path.as_deref(),
        Some("$HOMEBREW_PREFIX/var/log/foo.err")
    );
    assert_eq!(svc.interval, Some(3600));
    assert_eq!(svc.name.as_deref(), Some("com.example.foo"));
    // An interpolated run is beyond data-only parsing.
    let rb = "class Foo < Formula\n  service do\n    run [opt_bin/\"foo\", \"--opt=#{etc}/x\"]\n  end\nend\n";
    assert!(parse_rb_as_data(rb, "me/tap/foo").service.is_none());
}

#[test]
fn tap_name_parse() {
    assert_eq!(
        parse_tap_name("me/homebrew-tools"),
        Some(("me".into(), "tools".into()))
    );
    assert_eq!(parse_tap_name("bogus"), None);
    for invalid in [
        "../x",
        "./x",
        "/x",
        "me/..",
        "me/.",
        "me/homebrew-",
        "me/homebrew-..",
        "me/x/y",
        "me\\x/repo",
        "me/repo\\x",
        "me/repo\0",
    ] {
        assert_eq!(parse_tap_name(invalid), None, "{invalid:?}");
    }
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    p.ensure().unwrap();
    let outside = p.root.join("Library/homebrew-x");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("marker"), b"keep").unwrap();
    assert!(remove(&p, "../x").is_err());
    assert!(lookup(&p, "../x/marker").is_none());
    assert_eq!(std::fs::read(outside.join("marker")).unwrap(), b"keep");
    let missing = p.taps_dir().join("missing-user");
    assert!(remove(&p, "missing-user/x").is_err());
    assert!(!missing.exists(), "untap never creates a parent");
    let (ordinary, update) = mutation_path(&p, "me", "tools", true).unwrap();
    assert!(!update);
    std::fs::create_dir(&ordinary).unwrap();
    std::fs::create_dir(ordinary.join(".git")).unwrap();
    assert!(mutation_path(&p, "me", "tools", true).unwrap().1);
    std::fs::write(ordinary.join("payload"), b"ordinary").unwrap();
    remove(&p, "me/homebrew-tools").unwrap();
    assert!(!ordinary.exists());
    let regular = p.taps_dir().join("me/homebrew-regular");
    std::fs::write(&regular, b"keep regular").unwrap();
    assert!(remove(&p, "me/regular").is_err());
    assert!(mutation_path(&p, "me", "regular", true).is_err());
    assert_eq!(std::fs::read(&regular).unwrap(), b"keep regular");
    #[cfg(unix)]
    for (label, suffix) in [
        ("root", "Library/Taps/me/homebrew-x"),
        ("Library", "Taps/me/homebrew-x"),
        ("Taps", "me/homebrew-x"),
        ("user", "homebrew-x"),
        ("leaf", ""),
    ] {
        let p = Prefix::at(&tmp.path().join(label));
        p.ensure().unwrap();
        let external = tmp.path().join(format!("external-{label}"));
        let victim = external.join(suffix);
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("marker"), b"keep").unwrap();
        let alias = match label {
            "root" => p.root.clone(),
            "Library" => p.root.join("Library"),
            "Taps" => p.taps_dir(),
            "user" => p.taps_dir().join("me"),
            _ => p.taps_dir().join("me/homebrew-x"),
        };
        if let Some(parent) = alias.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        if alias.is_dir() {
            std::fs::remove_dir_all(&alias).unwrap();
        }
        std::os::unix::fs::symlink(&external, &alias).unwrap();
        assert!(remove(&p, "me/x").is_err(), "{label}");
        assert!(mutation_path(&p, "me", "x", true).is_err(), "{label}");
        assert_eq!(std::fs::read(victim.join("marker")).unwrap(), b"keep");
        assert_eq!(std::fs::read_link(&alias).unwrap(), external);
    }
    #[cfg(unix)]
    {
        let (path, _) = mutation_path(&p, "me", "worktree", true).unwrap();
        std::fs::create_dir(&path).unwrap();
        let external = tmp.path().join("external-git");
        std::fs::create_dir(&external).unwrap();
        std::fs::write(external.join("marker"), b"keep").unwrap();
        std::os::unix::fs::symlink(&external, path.join(".git")).unwrap();
        assert!(mutation_path(&p, "me", "worktree", true).is_err());
        assert_eq!(std::fs::read(external.join("marker")).unwrap(), b"keep");
    }
    assert_eq!(std::fs::read(outside.join("marker")).unwrap(), b"keep");
}

#[test]
fn migration_target_reads_local_tap_migrations() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    assert_eq!(migration_target_tap(&p, "example/tap", "aztfy"), None);
    let dir = p.taps_dir().join("example/homebrew-tap");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tap_migrations.json"),
        br#"{"aztfy": "homebrew/core", "old": "other/tap/renamed"}"#,
    )
    .unwrap();
    assert_eq!(
        migration_target_tap(&p, "example/tap", "aztfy").as_deref(),
        Some("homebrew/core")
    );
    assert_eq!(
        migration_target_tap(&p, "example/homebrew-tap", "old").as_deref(),
        Some("other/tap")
    );
    assert_eq!(migration_target_tap(&p, "example/tap", "missing"), None);
}

#[cfg(unix)]
#[test]
fn tap_link_hook_links_manpages_and_setting_gated_completions() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let root = p.taps_dir().join("someone/homebrew-tools");
    for dir in ["manpages", "completions/zsh", ".git"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("manpages/tool.1"), b"man").unwrap();
    std::fs::write(root.join("completions/zsh/_tool"), b"#compdef tool").unwrap();
    let tap = Tap {
        user: "someone".to_string(),
        repo: "tools".to_string(),
        path: root.clone(),
    };
    // Unlinked setting: manpages link, completions do not.
    link_completions_and_manpages(&p, &tap);
    assert!(p.root.join("share/man/man1/tool.1").exists());
    assert!(!p.root.join("share/zsh/site-functions/_tool").exists());
    assert!(completions_message_shown(&p));
    // Linked setting: completions follow.
    set_completions_linked(&p, true).unwrap();
    link_completions_and_manpages(&p, &tap);
    assert!(p.root.join("share/zsh/site-functions/_tool").exists());
    // Untap removes both link sets.
    remove(&p, "someone/tools").unwrap();
    assert!(!p.root.join("share/man/man1/tool.1").exists());
    assert!(!p.root.join("share/zsh/site-functions/_tool").exists());
    assert!(!root.exists());
}

#[test]
fn git_redirect_parsing_and_refusal_match_brew() {
    let stderr = "warning: redirecting to https://github.com/New/tools.git/\nFrom https://github.com/old/tools\n";
    assert_eq!(
        redirected_remotes(stderr).collect::<Vec<_>>(),
        vec!["https://github.com/New/tools.git/"]
    );
    // Non-ASCII prefix whose Unicode `.to_lowercase()` expands UTF-8 byte length (`İ` -> `i\u{307}`)
    // must not shift or panic the byte slice into `line`.
    assert_eq!(
        unsafe_redirect(
            "https://github.com/old/tools",
            "İhtar: Redirecting to https://github.com/New/tools.git/\n",
        )
        .as_deref(),
        Some("https://github.com/New/tools.git/")
    );
    // Multi-hop redirect where the first hop canonicalizes `.git/` (`same_remote`) and the
    // second hop crosses to a new repository must still be refused with the new target.
    let multi_hop = "warning: redirecting to https://github.com/old/tools.git/\n\
                     warning: redirecting to https://github.com/New/tools.git/\n";
    assert_eq!(
        unsafe_redirect("https://github.com/old/tools", multi_hop).as_deref(),
        Some("https://github.com/New/tools.git/")
    );
    assert_eq!(
        unsafe_redirect(
            "https://github.com/old/tools",
            "warning: redirecting to https://github.com/old/tools.git/\n",
        ),
        None
    );
    assert_eq!(redirected_remotes("From https://x\n").next(), None);
    assert!(same_remote(
        "git@github.com:Old/Tools.git",
        "https://github.com/old/tools/"
    ));
    assert!(same_remote(
        "https://github.com/old/tools",
        "https://github.com/old/tools.git"
    ));
    assert!(!same_remote(
        "https://github.com/old/tools",
        "https://github.com/new/tools"
    ));
    assert!(!same_remote("", "https://github.com/new/tools"));
    assert!(!same_remote("   ", "   "));
    assert_eq!(
        redirect_refusal("old/tools", "https://github.com/new/tools"),
        "Error: Refusing automatic redirect from old/tools to https://github.com/new/tools.\n\
         Untap old/tools, then tap the new location directly to review its source and trust."
    );
}
/// r384: resource stanzas parse in file order with
/// other-OS blocks uncollected; interpolation, a missing
/// url or an unterminated block refuse the file.
#[test]
fn resource_stanzas_parse_in_file_order() {
    let text = "class Py < Formula\n  resource \"wheel\" do\n    url \"https://files.pythonhosted.org/packages/a/wheel-0.45.0.tar.gz\"\n    sha256 \"x\"\n  end\n\n  on_linux do\n    resource \"linonly\" do\n      url \"https://example.com/linonly-1.0.tar.gz\"\n    end\n  end\n\n  resource \"packaging\" do\n    url \"https://files.pythonhosted.org/packages/b/packaging-26.3.tar.gz\"\n    version \"26.3\"\n  end\nend\n";
    let got = parse_resource_stanzas(text).unwrap();
    let names: Vec<&str> = got.iter().map(|(n, ..)| n.as_str()).collect();
    if cfg!(target_os = "linux") {
        assert_eq!(names, ["wheel", "linonly", "packaging"]);
    } else {
        assert_eq!(names, ["wheel", "packaging"]);
    }
    assert_eq!(got[0].2, None);
    assert_eq!(got[got.len() - 1].2.as_deref(), Some("26.3"));
    assert!(
        parse_resource_stanzas(
            "  resource \"a\" do\n    url \"https://x/#{version}.tgz\"\n  end\n"
        )
        .is_none()
    );
    assert!(parse_resource_stanzas("  resource \"a\" do\n    sha256 \"x\"\n  end\n").is_none());
    assert!(
        parse_resource_stanzas("  resource \"a\" do\n    url \"https://x/a-1.tgz\"\n").is_none()
    );
    assert_eq!(
        parse_resource_stanzas("class A < Formula\nend\n").unwrap(),
        Vec::new()
    );
    // r409: a git resource's `tag:` spec (same-line and
    // continuation forms) is captured, and a `:test`
    // resource header parses like any other.
    let text = "class G < Formula\n  resource \"sub\" do\n    url \"https://github.com/x/sub.git\",\n      tag: \"v1.2.3\",\n      revision: \"abc\"\n  end\n  resource \"data\", :test do\n    url \"https://example.com/data.wasm\"\n  end\nend\n";
    let got = parse_resource_stanzas(text).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].3.as_deref(), Some("v1.2.3"));
    assert_eq!(got[1].3, None);
    assert!(
        parse_resource_stanzas(
            "  resource \"a\" do\n    url \"https://x/a.git\", tag: \"#{v}\"\n  end\n"
        )
        .is_none()
    );
}

/// r388: `user_agent:` url options parse (multi-line and
/// same-line); any other option still sets `url_options`.
#[test]
fn livecheck_user_agent_option_parses() {
    let spec = parse_livecheck_block(
        "class M < Formula\n  livecheck do\n    url \"https://example.com/dl\",\n        user_agent: :browser\n    regex(/v(\\d+)/)\n  end\nend\n",
    )
    .unwrap();
    assert_eq!(spec.user_agent.as_deref(), Some(":browser"));
    assert!(!spec.url_options);
    assert_eq!(spec.regex.as_deref(), Some(r"v(\d+)"));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\", user_agent: \"Mozilla/9.9 Probe, Agent\"\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.user_agent.as_deref(), Some("Mozilla/9.9 Probe, Agent"));
    assert!(!spec.url_options);
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\", using: :post\n  end\n",
    )
    .unwrap();
    assert!(spec.url_options);
    assert_eq!(spec.user_agent, None);
    // r437: a multi-line `post_form:` hash parses in
    // written order (percona); `#{version...}` stays
    // verbatim for check time.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/ajax\", post_form: {\n      action:     \"percona_downloads\",\n      product_id: \"Percona-Server-#{version.major_minor}\",\n    }\n    regex(/v(\\d+)/)\n  end\n",
    )
    .unwrap();
    assert!(!spec.url_options);
    assert_eq!(
        spec.post_form,
        Some(vec![
            ("action".to_string(), "percona_downloads".to_string()),
            (
                "product_id".to_string(),
                "Percona-Server-#{version.major_minor}".to_string()
            ),
        ])
    );
    assert_eq!(spec.regex.as_deref(), Some(r"v(\d+)"));
    // Same-line form (ltl2ba shape).
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl.php\", post_form: { getx: \"Get X\" }\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.post_form,
        Some(vec![("getx".to_string(), "Get X".to_string())])
    );
    // A computed value does not reduce (option flags).
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\", post_form: { token: ENV[\"T\"] }\n  end\n",
    )
    .unwrap();
    assert!(spec.url_options);
    assert_eq!(spec.post_form, None);
}

/// r387: a tap formula's version detects from the file
/// name's version token, not digits inside the name.
#[test]
fn version_from_url_uses_version_token() {
    let f = parse_rb_as_data(
        "class Gate403 < Formula\n  url \"https://example.com/gate403-1.0.tar.gz\"\nend\n",
        "probeuser/probe/gate403",
    );
    assert_eq!(f.version, "1.0");
    let f = parse_rb_as_data(
        "class Foo < Formula\n  url \"https://example.com/foo-v2.10.0.tgz\"\nend\n",
        "probeuser/probe/foo",
    );
    assert_eq!(f.version, "2.10.0");
}

/// r385: a resource's own livecheck block is extracted and parsed,
/// and its `url` line is never mistaken for the resource's.
#[test]
fn resource_livecheck_block_parses() {
    use super::super::formula::LivecheckUrl;
    let text = "class Py < Formula\n  resource \"wheel\" do\n    livecheck do\n      url \"https://files.pythonhosted.org/packages/c/flit_core-9.9.tar.gz\"\n    end\n\n    url \"https://files.pythonhosted.org/packages/a/wheel-0.45.0.tar.gz\"\n    sha256 \"x\"\n  end\n\n  resource \"man\" do\n    url \"https://example.com/man-1.0.tar.gz\"\n\n    livecheck do\n      formula :parent\n    end\n  end\nend\n";
    let got = parse_resource_stanzas(text).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0].1,
        "https://files.pythonhosted.org/packages/a/wheel-0.45.0.tar.gz"
    );
    let spec = got[0].4.as_ref().expect("resource livecheck parsed");
    assert_eq!(
        spec.url,
        Some(LivecheckUrl::Literal(
            "https://files.pythonhosted.org/packages/c/flit_core-9.9.tar.gz".into()
        ))
    );
    let spec = got[1].4.as_ref().expect("parent ref parsed");
    assert_eq!(spec.formula_ref.as_deref(), Some(":parent"));
    // A livecheck block the package parser refuses (an unknown
    // statement) refuses the file.
    let bad = "  resource \"a\" do\n    url \"https://x/a-1.tgz\"\n\n    livecheck do\n      frobnicate \"x\"\n    end\n  end\n";
    assert!(parse_resource_stanzas(bad).is_none());
}

/// r397: `:json` strategy blocks reduce to data shapes for
/// the enumerable bodies; anything else stays block-only.
#[test]
fn json_strategy_block_shapes() {
    use super::super::formula::{JsonKey, JsonShape};
    let spec = |body: &str| {
        let text = format!(
            "class J < Formula\n  livecheck do\n    url \"https://example.com/x.json\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.json_block.map(|b| b.shape));
    assert_eq!(
        shape("    strategy :json do |json|\n      json[\"version\"]\n    end"),
        Some(JsonShape::Dig(vec![JsonKey::Key("version".into())]))
    );
    assert_eq!(
        shape("    strategy :json do |json|\n      json.dig(\"a\", \"b\", 0)\n    end"),
        Some(JsonShape::Dig(vec![
            JsonKey::Key("a".into()),
            JsonKey::Key("b".into()),
            JsonKey::Idx(0),
        ]))
    );
    assert_eq!(
        shape(
            "    strategy :json do |json|\n      json[\"releases\"]&.map { |item| item[\"version\"] }\n    end"
        ),
        Some(JsonShape::MapKey(
            vec![JsonKey::Key("releases".into())],
            Some("version".into())
        ))
    );
    assert_eq!(
        shape(
            "    strategy :json do |json, regex|\n      json.dig(\"a\", \"url\")&.[](regex, 1)\n    end"
        ),
        Some(JsonShape::DigRegex(vec![
            JsonKey::Key("a".into()),
            JsonKey::Key("url".into()),
        ]))
    );
    // r445: a regex shape without the regex
    // parameter reduces as a template (the oracle
    // feeds the spec regex through the DSL, the
    // git-block precedent).
    assert!(matches!(
        shape("    strategy :json do |json|\n      json[\"a\"]&.[](regex, 1)\n    end"),
        Some(JsonShape::Template(_))
    ));
    // A guarded body reduces since r400 (see
    // `json_guarded_map_shapes`).
    let s = spec(
        "    strategy :json do |json|\n      json[\"x\"]&.map do |item|\n        next if item[\"s\"] != \"y\"\n\n        item[\"version\"]\n      end\n    end",
    )
    .unwrap();
    assert!(s.strategy_block && s.json_block.is_some());
}

/// r398: `:xml` strategy blocks reduce to data shapes for
/// the enumerable bodies; anything else stays block-only.
#[test]
fn xml_strategy_block_shapes() {
    use super::super::formula::XmlShape;
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/x.xml\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.xml_block.map(|b| b.shape));
    assert_eq!(
        shape(
            "    strategy :xml do |xml|\n      xml.get_elements(\"//properties/ver\").map(&:text)\n    end"
        ),
        Some(XmlShape::Texts(vec!["properties".into(), "ver".into()]))
    );
    assert_eq!(
        shape(
            "    strategy :xml do |xml, regex|\n      xml.get_elements(\"//Contents/Key\").filter_map do |item|\n        item.text&.[](regex, 1)\n      end\n    end"
        ),
        Some(XmlShape::TextsRegex(vec!["Contents".into(), "Key".into()]))
    );
    // r465: a relative path reads as child steps
    // from the document (seekfast).
    assert!(matches!(
        shape(
            "    strategy :xml do |xml|\n      xml.get_elements(\"Contents/Key\").map(&:text)\n    end"
        ),
        Some(XmlShape::Template(_))
    ));
}

/// r399: `:yaml` strategy blocks reduce to data shapes for
/// the dig/index bodies; anything else stays block-only.
#[test]
fn yaml_strategy_block_shapes() {
    use super::super::formula::{JsonKey, YamlShape};
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/x.yaml\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.yaml_block.map(|b| b.shape));
    assert_eq!(
        shape(
            "    strategy :yaml do |yaml, regex|\n      yaml.dig(\"importers\", \".\", \"version\")&.[](regex, 1)\n    end"
        ),
        Some(YamlShape::DigRegex(vec![
            JsonKey::Key("importers".into()),
            JsonKey::Key(".".into()),
            JsonKey::Key("version".into()),
        ]))
    );
    assert_eq!(
        shape("    strategy :yaml do |yaml|\n      yaml.dig(\"items\", 1)\n    end"),
        Some(YamlShape::Dig(vec![
            JsonKey::Key("items".into()),
            JsonKey::Idx(1),
        ]))
    );
    // r472 foks/openttd: guarded item-key maps.
    assert!(matches!(
        shape(
            "    strategy :yaml do |yaml|\n      yaml[\"changelog\"]&.map { |r| r[\"version\"] }\n    end"
        ),
        Some(YamlShape::MapKey { .. })
    ));
    assert!(matches!(
        shape(
            "    strategy :yaml do |yaml|\n      yaml[\"latest\"]&.map do |item|\n        next if item[\"name\"] != \"stable\"\n        next if item[\"version\"].blank?\n\n        item[\"version\"].to_s\n      end\n    end"
        ),
        Some(YamlShape::MapKey { ref guards, .. }) if guards.len() == 2
    ));
    // r452 electron-builder forms: files map, root
    // match, version tails.
    {
        use super::super::formula::{YamlPart, YamlRender, YamlTail};
        assert_eq!(
            shape(
                "    strategy :electron_builder do |yaml, regex|\n      yaml[\"files\"]&.map do |item|\n        match = item[\"url\"]&.match(regex)\n        next if match.blank?\n\n        \"#{yaml[\"version\"]},#{match[1]}\"\n      end\n    end"
            ),
            Some(YamlShape::FilesMap {
                key: "url".into(),
                render: YamlRender::Template(vec![
                    YamlPart::Root("version".into()),
                    YamlPart::Lit(",".into()),
                    YamlPart::Cap(1),
                ]),
            })
        );
        assert_eq!(
            shape(
                "    strategy :electron_builder do |yaml, regex|\n      yaml[\"files\"]&.map do |item|\n        match = item[\"url\"]&.match(regex)\n        next if match.blank?\n\n        match[2].present? ? \"#{match[1]},#{match[2]}\" : match[1]\n      end\n    end"
            ),
            Some(YamlShape::FilesMap {
                key: "url".into(),
                render: YamlRender::PresentElse {
                    cap: 2,
                    then: vec![
                        YamlPart::Cap(1),
                        YamlPart::Lit(",".into()),
                        YamlPart::Cap(2),
                    ],
                    els: vec![YamlPart::Cap(1)],
                },
            })
        );
        assert_eq!(
            shape(
                "    strategy :electron_builder do |item, regex|\n      match = item[\"path\"]&.match(regex)\n      next if match.blank?\n\n      \"#{match[2]},#{match[1]}\"\n    end"
            ),
            Some(YamlShape::RootMatch {
                key: "path".into(),
                render: YamlRender::Template(vec![
                    YamlPart::Cap(2),
                    YamlPart::Lit(",".into()),
                    YamlPart::Cap(1),
                ]),
            })
        );
        assert_eq!(
            shape(
                "    strategy :electron_builder do |yaml|\n      yaml[\"version\"]&.delete_suffix(\"-ow\")\n    end"
            ),
            Some(YamlShape::DigTail(
                vec![JsonKey::Key("version".into())],
                YamlTail::DeleteSuffix("-ow".into()),
            ))
        );
        assert_eq!(
            shape(
                "    strategy :electron_builder do |yaml|\n      yaml[\"version\"]&.sub(/[._-]release.*$/i, \"\")\n    end"
            ),
            Some(YamlShape::DigTail(
                vec![JsonKey::Key("version".into())],
                YamlTail::SubRe("[._-]release.*$".into(), "i".into(), "".into()),
            ))
        );
    }
    // A computed body does not reduce.
    assert_eq!(
        shape(
            "    strategy :yaml do |yaml|\n      yaml.dig(\"a\").to_s.gsub(\"-\", \"_\")\n    end"
        ),
        None
    );
    // r430: multi-capture scan forms.
    let jn = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/\"\n    regex(/v(\\d+)(\\d+)/i)\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text).and_then(|s| s.page_block.map(|b| b.shape))
    };
    assert_eq!(
        jn(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match.join(\".\") }\n    end"
        ),
        Some(super::super::formula::PageShape::ScanJoin(".".into()))
    );
    assert_eq!(
        jn(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match[1] ? \"#{match[0]},#{match[1]}\" : match[0] }\n    end"
        ),
        Some(super::super::formula::PageShape::ScanCsvOpt)
    );
    // The regex-capture form needs the regex parameter.
    assert_eq!(
        shape("    strategy :yaml do |yaml|\n      yaml.dig(\"a\")&.[](regex, 1)\n    end"),
        None
    );
    // r422: `:electron_builder` blocks reduce the same way
    // (the oracle runs them through the Yaml strategy).
    assert_eq!(
        shape("    strategy :electron_builder do |yaml|\n      yaml.dig(\"version\")\n    end"),
        Some(YamlShape::Dig(vec![JsonKey::Key("version".into())]))
    );
}

/// r423: `page.scan(regex).map { |m| m[0].tr(..) }`
/// blocks reduce to a ScanTr data shape; anything
/// else in a page_match block stays block-only.
#[test]
fn page_match_tr_block_shapes() {
    use super::super::formula::{HopPick, HopScan, HopTarget, PageCond, PagePart, PageShape};
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/\"\n    regex(/v(\\d+)/i)\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.page_block.map(|b| b.shape));
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match[0].tr(\"_\", \".\") }\n    end"
        ),
        Some(PageShape::ScanTr("_".into(), ".".into()))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match&.first&.tr(\"_R\", \".r\") }\n    end"
        ),
        Some(PageShape::ScanTr("_R".into(), ".r".into()))
    );
    assert_eq!(
        shape(
            "    strategy :sourceforge do |page, regex|\n      page.scan(regex).map { |match| match.first.tr(\"_\", \".\") }\n    end"
        ),
        Some(PageShape::ScanTr("_".into(), ".".into()))
    );
    // r459: scan forks, split-chars, match fork.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map do |match|\n        (match[0] == match[1]) ? match[0] : \"#{match[0]},#{match[1]}\"\n      end\n    end"
        ),
        Some(PageShape::ScanFork {
            cond: PageCond::CapEq(0, 1),
            then: vec![PagePart::Cap(0)],
            els: vec![
                PagePart::Cap(0),
                PagePart::Lit(",".into()),
                PagePart::Cap(1)
            ],
        })
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match[0].split(\"\", 4).join(\".\") }\n    end"
        ),
        Some(PageShape::ScanSplitChars(4, ".".into()))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      match = page.match(regex)\n      next unless match\n      match[2].present? ? \"#{match[1]},#{match[2]}\" : match[1]\n    end"
        ),
        Some(PageShape::MatchFork {
            cond: PageCond::CapPresent(2),
            then: vec![
                PagePart::Cap(1),
                PagePart::Lit(",".into()),
                PagePart::Cap(2)
            ],
            els: vec![PagePart::Cap(1)],
        })
    );
    // r464: line-continued `.map`, count forks.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex)\n          .map { |match| \"#{match[0]},#{match[1]}\" }\n    end"
        ),
        Some(PageShape::ScanTemplate(vec![
            PagePart::Cap(0),
            PagePart::Lit(",".into()),
            PagePart::Cap(1),
        ]))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map do |match|\n        if (version = match[0]).count(\".\") >= 2\n          version\n        else\n          \"#{version}.0\"\n        end\n      end\n    end"
        ),
        Some(PageShape::ScanFork {
            cond: PageCond::CapCountGe(0, '.', 2),
            then: vec![PagePart::Cap(0)],
            els: vec![PagePart::Cap(0), PagePart::Lit(".0".into())],
        })
    );
    // r467: guarded forks, split captures,
    // parse_json digs, two-match templates.
    assert!(matches!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).filter_map do |match|\n        next if match[1]&.match?(/^RC/i)\n        match[1].present? ? \"#{match[0]},#{match[1]}\" : match[0]\n      end\n    end"
        ),
        Some(PageShape::ScanGuardedFork { skip_cap: 1, .. })
    ));
    assert!(matches!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.split(/<h\\d+/i).filter_map do |section|\n        next if section.match?(/Canary/i)\n        section[regex, 1]\n      end\n    end"
        ),
        Some(PageShape::SplitCapture { .. })
    ));
    assert!(matches!(
        shape(
            "    strategy :page_match do |page, regex|\n      match = page.match(regex)\n      next if match.blank?\n      json = Homebrew::Livecheck::Strategy::Json.parse_json(match[1])\n      json[\"buildVersion\"]\n    end"
        ),
        Some(PageShape::MatchJsonDig { cap: 1, .. })
    ));
    let two = parse_livecheck_block(
        "  livecheck do\n    url :homepage\n    strategy :page_match do |page|\n      version = page.match(/v(\\d+)/i)\n      metaurl = page.match(%r{/(\\d+)/x}i)\n      next if version.blank? || metaurl.blank?\n      \"#{version[1]},#{metaurl[1]}\"\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        two.page_block.map(|b| b.shape),
        Some(PageShape::TwoMatch { .. })
    ));
    // r468 hermes-desktop: the first match may use
    // the block regex (`page.match(regex)`), recorded
    // as an empty `a_pat` marker.
    assert!(matches!(
        shape(
            "    strategy :page_match do |page, regex|\n      version_match = page.match(regex)\n      next unless version_match\n      build_match = page.match(/Setup\\.dmg\\?build=(\\h+)/i)\n      next unless build_match\n      \"#{version_match[1]},#{build_match[1]}\"\n    end"
        ),
        Some(PageShape::TwoMatch { a_pat, .. }) if a_pat.is_empty()
    ));
    // r468 imagej: a leading capture-alias statement
    // in a scan map body rewrites to the capture.
    assert!(matches!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map do |match|\n        version = match[0]\n        version.include?(\".\") ? version : version.sub(/(\\d)(\\d+)/, '\\\\1.\\\\2')\n      end\n    end"
        ),
        Some(PageShape::ScanFork {
            cond: PageCond::CapIncludes(0, _),
            ..
        })
    ));
    // r470: an undeclared `regex` resolves to the
    // livecheck's own regex (sleep-aid's `|page|`
    // scan runs under the oracle), so the shape
    // reduces just like the declared-param form.
    // (Deliberately reversed from the r423 rule.)
    assert_eq!(
        shape(
            "    strategy :page_match do |page|\n      page.scan(regex).map { |match| match[0].tr(\"_\", \".\") }\n    end"
        ),
        Some(PageShape::ScanTr("_".into(), ".".into()))
    );
    // r470 icollections: split-head char window.
    assert_eq!(
        shape(
            "    strategy :page_match do |page|\n      page.split(\":\").first[0..2].chars.join(\".\")\n    end"
        ),
        Some(PageShape::SplitHeadChars {
            sep: ":".into(),
            start: 0,
            len: 3,
            join: ".".into()
        })
    );
    // A computed map body does not reduce.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| Date.parse(match.first) }\n    end"
        ),
        None
    );
    // r424: Date.parse("%Y%m%d") bodies reduce, direct
    // and assign-then-ternary forms.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| Date.parse(match.first)&.strftime(\"%Y%m%d\") }\n    end"
        ),
        Some(PageShape::ScanDateYmd)
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map do |match|\n        date_str = match&.first\n        date_str ? Date.parse(date_str)&.strftime(\"%Y%m%d\") : nil\n      end\n    end"
        ),
        Some(PageShape::ScanDateYmd)
    );
    // Another strftime format does not reduce.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| Date.parse(match.first)&.strftime(\"%Y-%m-%d\") }\n    end"
        ),
        None
    );
    // r436 objconv: first capture parsed as a date,
    // dashed rendering.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      date = page[regex, 1]\n      next if date.blank?\n\n      Date.parse(date).strftime(\"%Y-%m-%d\")\n    end"
        ),
        Some(PageShape::FirstDate(true))
    );
    // r436 vip: match form, basic rendering.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      match = page.match(regex)\n      next if match.blank?\n\n      Date.parse(match[1])&.strftime(\"%Y%m%d\")\n    end"
        ),
        Some(PageShape::FirstDate(false))
    );
    // A different strftime does not reduce.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      date = page[regex, 1]\n      next if date.blank?\n\n      Date.parse(date).strftime(\"%d.%m.%Y\")\n    end"
        ),
        None
    );
    // r439 libgweather: scan-select under a version
    // bound, flattened.
    assert_eq!(
        shape(
            "    strategy :gnome do |page, regex|\n      page.scan(regex).select { |match| Version.new(match.first) < 40 }.flatten\n    end"
        ),
        Some(PageShape::ScanSelectLt("40".into()))
    );
    // r440 kytea: literal first capture, join a
    // literal base, scan all.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      index_file_path = page[/src=[\"']?([^\"' >]*?index\\.js)/i, 1]\n      next unless index_file_path\n\n      js_content = Homebrew::Livecheck::Strategy.page_content(\n        URI.join(\"https://example.com/app/\", index_file_path).to_s,\n      )[:content]\n      next if js_content.blank?\n\n      js_content.scan(regex).map { |match| match[0] }\n    end"
        ),
        Some(PageShape::HopScan(Box::new(HopScan {
            hop_re: Some(("src=[\"']?([^\"' >]*?index\\.js)".into(), "i".into())),
            pick: HopPick::First,
            target: HopTarget::JoinLiteral("https://example.com/app/".into()),
            last_only: false,
        })))
    );
    // r440 dovecot: block-regex scan last, join
    // literal, last capture only.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      major_minor = page.scan(regex)&.flatten&.last\n      next if major_minor.blank?\n\n      version_page = Homebrew::Livecheck::Strategy.page_content(\n        URI.join(\"https://example.com/releases/\", major_minor).to_s,\n      )\n      next if version_page[:content].blank?\n\n      version_page[:content].scan(regex)&.flatten&.last\n    end"
        ),
        Some(PageShape::HopScan(Box::new(HopScan {
            hop_re: None,
            pick: HopPick::ScanLast,
            target: HopTarget::JoinLiteral("https://example.com/releases/".into()),
            last_only: true,
        })))
    );
    // r440 gnutls: literal scan maxed by version,
    // interpolated target, assign-in-guard content.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      highest_version = page.scan(%r{href=.*?/gnutls/v?(\\d+(?:\\.\\d+)+)/?[\"' >]}i)\n                            .map { |match| match[0] }\n                            .max_by { |v| Version.new(v) }\n      next unless highest_version\n\n      files_page = Homebrew::Livecheck::Strategy.page_content(\n        \"https://example.com/gnutls/v#{highest_version}\",\n      )\n      next if (files_page_content = files_page[:content]).blank?\n\n      files_page_content.scan(regex).map { |match| match[0] }\n    end"
        ),
        Some(PageShape::HopScan(Box::new(HopScan {
            hop_re: Some((
                "href=.*?/gnutls/v?(\\d+(?:\\.\\d+)+)/?[\"' >]".into(),
                "i".into()
            )),
            pick: HopPick::ScanMax,
            target: HopTarget::Template("https://example.com/gnutls/v".into(), String::new()),
            last_only: false,
        })))
    );
    // r425: literal gsub, regex sub with literal
    // replacement, and the reject-one-version form.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match[0].gsub(\"_rev_\", \".\") }\n    end"
        ),
        Some(PageShape::ScanGsub("_rev_".into(), ".".into()))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match&.first&.sub(/%2Bdbg/i, \"\") }\n    end"
        ),
        Some(PageShape::ScanSub("%2Bdbg".into(), "i".into(), "".into()))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| ((version = match.first) == \"2.22\") ? nil : version }\n    end"
        ),
        Some(PageShape::ScanRejectEq("2.22".into()))
    );
    // r431: backreference replacements reduce
    // (r425 left them block-only), as do the
    // tr+sub chain and digit-chunk scan.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match[0].sub(/x/, '\\1') }\n    end"
        ),
        Some(PageShape::ScanSubBackref(
            "x".into(),
            "".into(),
            "\\1".into()
        ))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match.first.sub(/^(\\d)(\\d+)$/, '\\1.\\2') }\n    end"
        ),
        Some(PageShape::ScanSubBackref(
            "^(\\d)(\\d+)$".into(),
            "".into(),
            "\\1.\\2".into()
        ))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map do |match|\n        match[0].tr(\"~\", \"_\").sub(/\\.(\\h{7})$/, '-\\1')\n      end\n    end"
        ),
        Some(PageShape::ScanTrSubBackref(
            "~".into(),
            "_".into(),
            "\\.(\\h{7})$".into(),
            "".into(),
            "-\\1".into()
        ))
    );
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match[0].scan(/\\d{2}/).map(&:to_i).join(\".\") }\n    end"
        ),
        Some(PageShape::ScanChunks(2, ".".into()))
    );
    // Negated tr sets do not reduce.
    assert_eq!(
        shape(
            "    strategy :page_match do |page, regex|\n      page.scan(regex).map { |match| match[0].tr(\"^a\", \".\") }\n    end"
        ),
        None
    );
}

/// r426: `:github_releases` blocks reduce to data
/// shapes (the seven core bodies; calabash's
/// cross-formula suffix stays block-only).
#[test]
fn github_releases_block_shapes() {
    use super::super::formula::{
        GhRelBlock, GhRelCapture, GhRelLocalOp, GhRelMatchGuard, GhRelOp, GhRelPart, GhRelSource,
        GhRelThen, GhRelXform,
    };
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url :stable\n    regex(/^v?(\\d+(?:[.-]\\d+)+)$/i)\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.ghrel_block);
    let base = GhRelBlock {
        skip_draft: true,
        skip_prerelease: true,
        require_prerelease: false,
        tag_ne: None,
        tag_prefix: None,
        asset_any: None,
        name_skip: None,
        source: GhRelSource::TagName,
        pre_capture: None,
        then: GhRelThen::Identity,
        fallback_tag: None,
        blank_emit_local: false,
        limit: None,
        own_guards: Vec::new(),
    };
    // saxon: tr on the capture.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        match = release[\"tag_name\"]&.match(regex)\n        next if match.blank?\n\n        match[1]&.tr(\"-\", \".\")\n      end\n    end"
        ),
        Some(GhRelBlock {
            then: GhRelThen::Tr("-".into(), ".".into()),
            ..base.clone()
        })
    );
    // kpt: name guard.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        next if release[\"name\"]&.match?(/^api/i)\n\n        match = release[\"tag_name\"]&.match(regex)\n        next if match.blank?\n\n        match[1]\n      end\n    end"
        ),
        Some(GhRelBlock {
            name_skip: Some(("^api".into(), "i".into())),
            ..base.clone()
        })
    );
    // polynote: prerelease kept (commented-out guard).
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] # || release[\"prerelease\"]\n\n        match = release[\"tag_name\"]&.match(regex)\n        next if match.blank?\n\n        match[1]\n      end\n    end"
        ),
        Some(GhRelBlock {
            skip_prerelease: false,
            ..base.clone()
        })
    );
    // portable-openssl: non-zero patch via filter_map.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.filter_map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        match = release[\"tag_name\"]&.match(regex)\n        next if match.blank?\n\n        version = Version.new(match[1])\n        next if version.patch.to_i.zero?\n\n        version\n      end\n    end"
        ),
        Some(GhRelBlock {
            then: GhRelThen::NonZeroPatch,
            ..base.clone()
        })
    );
    // core-lightning: asset download URLs, flattened.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"browser_download_url\"]&.match(regex)\n          next if match.blank?\n\n          match[1]\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetDownloadUrl,
            ..base.clone()
        })
    );
    // r451: asset names + CSV render.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"name\"]&.match(regex)\n          next if match.blank?\n\n          \"#{match[1]},#{match[2]}\"\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetName,
            then: GhRelThen::Template(vec![
                GhRelPart::Cap(1),
                GhRelPart::Lit(",".into()),
                GhRelPart::Cap(2),
            ]),
            ..base.clone()
        })
    );
    // r451: prerelease-only bodies.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next unless release[\"prerelease\"]\n\n        match = release[\"tag_name\"]&.match(regex)\n        next if match.blank?\n\n        match[1]\n      end\n    end"
        ),
        Some(GhRelBlock {
            skip_draft: false,
            skip_prerelease: false,
            require_prerelease: true,
            ..base.clone()
        })
    );
    // r451: pre-capture local + EqElse against it.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        tag_version = release[\"tag_name\"]&.[](/^v?(\\d+(?:\\.\\d+)+)$/i, 1)\n        next if tag_version.blank?\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"name\"]&.match(regex)\n          next if match.blank?\n\n          (match[1] == tag_version) ? tag_version : \"#{match[1]},#{tag_version}\"\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetName,
            pre_capture: Some(GhRelCapture {
                field: "tag_name".into(),
                lit: Some((r"^v?(\d+(?:\.\d+)+)$".into(), "i".into())),
                op: GhRelLocalOp::Capture,
                blank_guard: true,
            }),
            then: GhRelThen::EqElse {
                a: GhRelOp::Cap(1),
                b: GhRelOp::Local,
                then: vec![GhRelPart::Local],
                els: vec![
                    GhRelPart::Cap(1),
                    GhRelPart::Lit(",".into()),
                    GhRelPart::Local,
                ],
            },
            ..base.clone()
        })
    );
    // r451: pre-capture only (name capture + tag tr).
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        version = release[\"name\"]&.[](regex, 1)\n        next if version.blank?\n\n        \"#{version},#{release[\"tag_name\"]&.tr(\"v\", \"\")}\"\n      end\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::PreCaptureOnly,
            pre_capture: Some(GhRelCapture {
                field: "name".into(),
                lit: None,
                op: GhRelLocalOp::Capture,
                blank_guard: true,
            }),
            then: GhRelThen::Template(vec![
                GhRelPart::Local,
                GhRelPart::Lit(",".into()),
                GhRelPart::TagTr("v".into(), "".into()),
            ]),
            ..base.clone()
        })
    );
    // r451: filter_map assets with `.presence ||` tag fallback.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"]\n\n        release[\"assets\"]&.filter_map do |asset|\n          match = asset[\"browser_download_url\"]&.match(regex)\n          next if match.blank?\n\n          match[1]\n        end.presence || release[\"tag_name\"]&.[](/v?(\\d+)/, 1)\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            skip_prerelease: false,
            source: GhRelSource::AssetDownloadUrl,
            fallback_tag: Some((r"v?(\d+)".into(), "".into())),
            ..base.clone()
        })
    );
    // r455: file_regex + asset_any guard, tag flatten.
    {
        use super::super::formula::GhRelAny;
        assert_eq!(
            shape(
                "    strategy :github_releases do |json, regex|\n      file_regex = /Wine\\sStable[^/]*\\.tar\\.xz/i\n      json.map do |release|\n        next if release[\"draft\"]\n        next unless release[\"assets\"]&.any? { |asset| asset[\"name\"]&.match?(file_regex) }\n\n        match = release[\"tag_name\"]&.match(regex)\n        next if match.blank?\n\n        match[1]\n      end.flatten\n    end"
            ),
            Some(GhRelBlock {
                skip_prerelease: false,
                asset_any: Some(GhRelAny::NameMatch(
                    r"Wine\sStable[^/]*\.tar\.xz".into(),
                    "i".into(),
                )),
                ..base.clone()
            })
        );
        // r455: truthiness fork render.
        assert_eq!(
            shape(
                "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        match = release[\"tag_name\"]&.match(regex)\n        next if match.blank?\n\n        match[2] ? \"#{match[1]}_#{match[2]}\" : match[1]\n      end\n    end"
            ),
            Some(GhRelBlock {
                then: GhRelThen::TruthyElse {
                    cap: 2,
                    then: vec![
                        GhRelPart::Cap(1),
                        GhRelPart::Lit("_".into()),
                        GhRelPart::Cap(2),
                    ],
                    els: vec![GhRelPart::Cap(1)],
                },
                ..base.clone()
            })
        );
        // r455: assets early-next against the raw tag.
        assert_eq!(
            shape(
                "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"]  || release[\"prerelease\"]\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"name\"]&.match(regex)\n          next if match.blank?\n          next match[1] if match[1] == release[\"tag_name\"]\n\n          \"#{match[1]},#{release[\"tag_name\"]}\"\n        end\n      end.flatten\n    end"
            ),
            Some(GhRelBlock {
                source: GhRelSource::AssetName,
                then: GhRelThen::EqElse {
                    a: GhRelOp::Cap(1),
                    b: GhRelOp::Tag,
                    then: vec![GhRelPart::Cap(1)],
                    els: vec![
                        GhRelPart::Cap(1),
                        GhRelPart::Lit(",".into()),
                        GhRelPart::Tag,
                    ],
                },
                ..base.clone()
            })
        );
    }
    // calabash: cross-formula state does not reduce.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      saxon_suffix = \"-1\"\n\n      json.map do |release|\n        next if release[\"draft\"]\n\n        match[1] if match[1].end_with?(saxon_suffix)\n      end\n    end"
        ),
        None
    );
    // r476: librecad — delete_prefix local, blank-emit.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        tag = release[\"tag_name\"]&.delete_prefix(\"v\")\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"name\"]&.match(regex)\n          next tag if match.blank?\n\n          (match[1] == tag) ? tag : \"#{tag},#{match[1]}\"\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetName,
            pre_capture: Some(GhRelCapture {
                field: "tag_name".into(),
                lit: None,
                op: GhRelLocalOp::DeletePrefix("v".into()),
                blank_guard: false,
            }),
            then: GhRelThen::EqElse {
                a: GhRelOp::Cap(1),
                b: GhRelOp::Local,
                then: vec![GhRelPart::Local],
                els: vec![
                    GhRelPart::Local,
                    GhRelPart::Lit(",".into()),
                    GhRelPart::Cap(1),
                ],
            },
            blank_emit_local: true,
            ..base.clone()
        })
    );
    // r476: wljs-notebook — raw-tag alias + capture off it.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        tag = release[\"tag_name\"]\n        tag_version = tag[/^v?(\\d+(?:\\.\\d+)+.*)$/i, 1]\n        next if tag_version.blank?\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"name\"]&.match(regex)\n          next if match.blank?\n\n          (match[1] == tag_version) ? tag_version : \"#{match[1]},#{tag}\"\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetName,
            pre_capture: Some(GhRelCapture {
                field: "tag_name".into(),
                lit: Some((r"^v?(\d+(?:\.\d+)+.*)$".into(), "i".into())),
                op: GhRelLocalOp::Capture,
                blank_guard: true,
            }),
            then: GhRelThen::EqElse {
                a: GhRelOp::Cap(1),
                b: GhRelOp::Local,
                then: vec![GhRelPart::Local],
                els: vec![
                    GhRelPart::Cap(1),
                    GhRelPart::Lit(",".into()),
                    GhRelPart::Tag,
                ],
            },
            ..base.clone()
        })
    );
    // r476: synfigstudio — compacted capture join.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"name\"]&.match(regex)\n          next if match.blank?\n\n          match.to_a.compact.drop(1).join(\",\")\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetName,
            then: GhRelThen::CompactJoin(",".into()),
            ..base.clone()
        })
    );
    // r476: outfox — sliced release list. (The comment
    // quoting `json.map do |release|` must not distort
    // block capture; see the probe in commands tests.)
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      json[0...9].map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"browser_download_url\"]&.match(regex)\n          next if match.blank?\n\n          \"#{match[3]},#{match[1]},#{match[2]},#{match[4]}\"\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetDownloadUrl,
            then: GhRelThen::Template(vec![
                GhRelPart::Cap(3),
                GhRelPart::Lit(",".into()),
                GhRelPart::Cap(1),
                GhRelPart::Lit(",".into()),
                GhRelPart::Cap(2),
                GhRelPart::Lit(",".into()),
                GhRelPart::Cap(4),
            ]),
            limit: Some(9),
            ..base.clone()
        })
    );
    // r476: free-gpgmail — own-version guards.
    assert_eq!(
        shape(
            "    strategy :github_releases do |json, regex|\n      version_suffix = version.csv.third&.sub(/^[_-]/, \"\")\n\n      json.map do |release|\n        next if release[\"draft\"] || release[\"prerelease\"]\n\n        release[\"assets\"]&.map do |asset|\n          match = asset[\"name\"]&.match(regex)\n          next if match.blank?\n          next if match[1].split(\".\").first != version.csv.first.split(\".\").first\n          next if match[3]&.sub(/^[_-]/, \"\") != version_suffix\n\n          \"#{match[1]},#{match[2]},#{match[3]}\"\n        end\n      end.flatten\n    end"
        ),
        Some(GhRelBlock {
            source: GhRelSource::AssetName,
            then: GhRelThen::Template(vec![
                GhRelPart::Cap(1),
                GhRelPart::Lit(",".into()),
                GhRelPart::Cap(2),
                GhRelPart::Lit(",".into()),
                GhRelPart::Cap(3),
            ]),
            own_guards: vec![
                GhRelMatchGuard {
                    cap: 1,
                    cap_xform: GhRelXform::SplitFirst(".".into()),
                    own_csv: 0,
                    own_xform: GhRelXform::SplitFirst(".".into()),
                },
                GhRelMatchGuard {
                    cap: 3,
                    cap_xform: GhRelXform::SubRe("^[_-]".into(), "".into(), "".into()),
                    own_csv: 2,
                    own_xform: GhRelXform::SubRe("^[_-]".into(), "".into(), "".into()),
                },
            ],
            ..base.clone()
        })
    );
}

/// r427: cask conditional locals (`on_arch_conditional`,
/// `arch`/`os` stanzas) and `Time.now.to_i` cache
/// busters substitute in livecheck url/regex at load;
/// the `version` family stays url-only.
#[test]
fn cask_livecheck_locals_substitute() {
    use super::super::formula::LivecheckUrl;
    let text = "cask \"x\" do\n  arch arm: \"apple\", intel: \"intel\"\n  folder = on_arch_conditional arm: \"arm64\", intel: \"x86_64\"\n  livecheck do\n    url \"https://example.com/updates_#{folder}.xml?noCache=#{Time.now.to_i}\"\n    regex(/app-v(\\d+)-#{arch}\\.dmg/i)\n  end\nend\n";
    let spec = parse_livecheck_block(text).unwrap();
    let Some(LivecheckUrl::Literal(url)) = &spec.url else {
        panic!("url literal expected, got {:?}", spec.url);
    };
    // This host is arm64 or x86_64; either way both
    // tokens resolved and no interpolation remains.
    assert!(url.starts_with("https://example.com/updates_"), "{url}");
    assert!(url.contains(".xml?noCache=1"), "{url}");
    assert!(!url.contains("#{"), "{url}");
    let re = spec.regex.unwrap();
    assert!(
        re == "app-v(\\d+)-apple\\.dmg" || re == "app-v(\\d+)-intel\\.dmg",
        "{re}"
    );
    // A nested conditional as a kwarg value resolves too.
    let text = "cask \"x\" do\n  arch arm: \"arm64\", intel: on_system_conditional(macos: \"x64\", linux: \"x86_64\")\n  livecheck do\n    url \"https://example.com/#{arch}/latest.yml\"\n  end\nend\n";
    let spec = parse_livecheck_block(text).unwrap();
    assert!(
        matches!(&spec.url, Some(LivecheckUrl::Literal(u)) if !u.contains("#{")),
        "{:?}",
        spec.url
    );
    // Unknown locals still refuse the whole block.
    assert!(
        parse_livecheck_block(
            "cask \"x\" do\n  livecheck do\n    url \"https://example.com/#{nope}/\"\n  end\nend\n"
        )
        .is_none()
    );
}

/// r429: dig bodies ending `.split("-")&.first`
/// reduce for :json and :yaml/:electron_builder.
#[test]
fn dig_split_first_shapes() {
    use super::super::formula::{JsonKey, JsonShape, YamlShape};
    let spec = |url: &str, body: &str| {
        let text =
            format!("class X < Formula\n  livecheck do\n    url \"{url}\"\n{body}\n  end\nend\n");
        parse_livecheck_block(&text)
    };
    let j = spec(
        "https://example.com/x.json",
        "    strategy :json do |json|\n      json[\"version\"]&.split(\"-\")&.first\n    end",
    )
    .and_then(|s| s.json_block.map(|b| b.shape));
    assert_eq!(
        j,
        Some(JsonShape::DigSplitFirst(
            vec![JsonKey::Key("version".into())],
            "-".into()
        ))
    );
    let y = spec(
        "https://example.com/latest-mac.yml",
        "    strategy :electron_builder do |yaml|\n      yaml[\"version\"]&.split(\"-\")&.first\n    end",
    )
    .and_then(|s| s.yaml_block.map(|b| b.shape));
    assert_eq!(
        y,
        Some(YamlShape::DigSplitFirst(
            vec![JsonKey::Key("version".into())],
            "-".into()
        ))
    );
    // `.last` does not reduce.
    let n = spec(
        "https://example.com/x.json",
        "    strategy :json do |json|\n      json[\"version\"]&.split(\"-\")&.last\n    end",
    )
    .and_then(|s| s.json_block.map(|b| b.shape));
    assert_eq!(n, None);
}

/// r432: the three two-stage directory-walk bodies
/// (gnu-getopt, texlive, singular) reduce to DirWalk.
#[test]
fn dirwalk_block_shapes() {
    use super::super::formula::{DirBlank, PageShape};
    let spec = |url: &str, re: &str, body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"{url}\"\n    regex({re})\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text).and_then(|s| s.page_block.map(|b| b.shape))
    };
    let gnu = spec(
        "https://example.org/util-linux/",
        "/href=.*?util-linux[._-]v?(\\d+(?:\\.\\d+)+)\\.t/i",
        "    strategy :page_match do |page, regex|\n      versions = page.scan(%r{href=[\"']?v?(\\d+(?:\\.\\d+)+)/?[\"' >]}i)\n                     .flatten\n                     .uniq\n                     .sort_by { |v| Version.new(v) }\n      next versions if versions.blank?\n\n      dir_versions = []\n      versions[-2..].reverse_each do |version|\n        dir_page = Homebrew::Livecheck::Strategy.page_content(\n          URI.join(@url, \"v#{version}/\").to_s,\n        )\n        next versions if dir_page[:content].blank?\n\n        dir_versions = dir_page[:content].scan(regex).flatten\n        break unless dir_versions.empty?\n      end\n\n      dir_versions.presence || versions\n    end",
    );
    let Some(PageShape::DirWalk(w)) = gnu else {
        panic!("gnu-getopt dirwalk, got {gnu:?}")
    };
    assert!(w.last_two && w.empty_scan_stage1 && w.blank == DirBlank::NextCandidate);
    assert_eq!((w.dir_pre.as_str(), w.dir_post.as_str()), ("v", "/"));
    assert!(w.stage1_regex.is_some() && w.stage2_regex.is_none());

    let tex = spec(
        "https://example.org/texlive/",
        "/href=.*?texlive[._-]v?(\\d+(?:\\.\\d+)*)[._-]source\\.t/i",
        "    strategy :page_match do |page, regex|\n      years = page.scan(%r{href=[\"']?v?(\\d+(?:\\.\\d+)*)/?[\"' >]}i)\n                  .flatten\n                  .uniq\n                  .map { |v| Version.new(v) }\n                  .sort\n      next if years.blank?\n\n      newest_year = years.last.to_s\n      year_page = Homebrew::Livecheck::Strategy.page_content(URI.join(@url, newest_year).to_s)\n      next if year_page[:content].blank?\n\n      year_page[:content].scan(regex).flatten\n    end",
    );
    let Some(PageShape::DirWalk(w)) = tex else {
        panic!("texlive dirwalk, got {tex:?}")
    };
    assert!(!w.last_two && w.blank == DirBlank::Nothing && !w.empty_scan_stage1);

    let sing = spec(
        "https://example.org/SOURCES/",
        "%r{href=[\"']?v?(\\d+(?:[.-]\\d+)+)/?[\"' >]}i",
        "    strategy :page_match do |page, regex|\n      versions = page.scan(regex)\n                     .flatten\n                     .uniq\n                     .map { |v| Version.new(v.tr(\"-\", \".\")) }\n                     .reject { |v| v.patch >= 90 }\n                     .sort\n      next versions if versions.blank?\n\n      newest_version = versions.last\n\n      dir_page = Homebrew::Livecheck::Strategy.page_content(\n        URI.join(@url, \"#{newest_version.to_s.tr(\".\", \"-\")}/\").to_s,\n      )\n      next versions if dir_page[:content].blank?\n\n      dir_versions = dir_page[:content].scan(/href=.*?singular[._-]v?(\\d+(?:\\.\\d+)+(?:p\\d+)?)\\.t/i).flatten\n\n      dir_versions || versions\n    end",
    );
    let Some(PageShape::DirWalk(w)) = sing else {
        panic!("singular dirwalk, got {sing:?}")
    };
    assert_eq!(w.reject_patch_ge, Some(90));
    assert_eq!(w.stage1_tr, Some(("-".into(), ".".into())));
    assert_eq!(w.dir_tr, Some((".".into(), "-".into())));
    assert!(w.blank == DirBlank::Stage1 && w.stage2_regex.is_some());
}

/// r400: `:json` guarded map bodies reduce to data:
/// `next if/unless` guards (or one postfix guard) over
/// enumerable field conditions around a simple
/// projection.
#[test]
fn json_guarded_map_shapes() {
    use super::super::formula::{
        JsonCond, JsonGuard, JsonKey, JsonLit, JsonShape, MapCapture, MapGuard,
    };
    let shape = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url \"https://example.com/x.json\"\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text).and_then(|s| s.json_block.map(|b| b.shape))
    };
    assert_eq!(
        shape(
            "    strategy :json do |json|\n      json[\"releases\"]&.map do |release|\n        next unless release[\"release_number\"]&.start_with?(version.major_minor)\n        next if release[\"status\"] != \"stable\"\n\n        release[\"release_number\"]\n      end\n    end"
        ),
        Some(JsonShape::MapGuarded(
            vec![JsonKey::Key("releases".into())],
            Box::new(MapGuard {
                item_key: Some("release_number".into()),
                capture: MapCapture::None,
                tr: None,
                sub: None,
                guards: vec![
                    JsonGuard {
                        unless: true,
                        cond: JsonCond::StartsWithMajorMinor("release_number".into()),
                    },
                    JsonGuard {
                        unless: false,
                        cond: JsonCond::Ne("status".into(), JsonLit::Str("stable".into())),
                    },
                ],
            }),
        ))
    );
    // r435 go: `none?` filename-match guard and a
    // literal-regex capture projection.
    assert_eq!(
        shape(
            "    strategy :json do |json, regex|\n      json.map do |release|\n        next if release[\"stable\"] != true\n        next if release[\"files\"].none? { |file| file[\"filename\"].match?(regex) }\n\n        release[\"version\"][/(\\d+(?:\\.\\d+)+)/, 1]\n      end\n    end"
        ),
        Some(JsonShape::MapGuarded(
            vec![],
            Box::new(MapGuard {
                item_key: Some("version".into()),
                capture: MapCapture::Literal("(\\d+(?:\\.\\d+)+)".into(), String::new()),
                tr: None,
                sub: None,
                guards: vec![
                    JsonGuard {
                        unless: false,
                        cond: JsonCond::Ne("stable".into(), JsonLit::Bool(true)),
                    },
                    JsonGuard {
                        unless: false,
                        cond: JsonCond::NoneMatch("files".into(), "filename".into()),
                    },
                ],
            }),
        ))
    );
    // r437 percona-server: unguarded filter_map over
    // a dig path, capture with a backreference sub.
    assert_eq!(
        shape(
            "    strategy :json do |json, regex|\n      json.dig(\"data\", \"versions\")&.filter_map do |version|\n        version[regex, 1]&.sub(/(-\\d+)\\.0$/, '\\1')\n      end\n    end"
        ),
        Some(JsonShape::MapGuarded(
            vec![JsonKey::Key("data".into()), JsonKey::Key("versions".into())],
            Box::new(MapGuard {
                item_key: None,
                capture: MapCapture::BlockRegex,
                tr: None,
                sub: Some(("(-\\d+)\\.0$".into(), String::new(), "\\1".into())),
                guards: vec![],
            }),
        ))
    );
    // r438 mariadb-connector-c: hash-values nested
    // guarded map, flattened.
    assert_eq!(
        shape(
            "    strategy :json do |json|\n      json[\"releases\"]&.map do |_, group|\n        group[\"children\"]&.map do |release|\n          next if release[\"status\"] != \"stable\"\n\n          release[\"release_number\"]\n        end\n      end&.flatten\n    end"
        ),
        Some(JsonShape::ValuesMap(
            vec![JsonKey::Key("releases".into())],
            "children".into(),
            Box::new(MapGuard {
                item_key: Some("release_number".into()),
                capture: MapCapture::None,
                tr: None,
                sub: None,
                guards: vec![JsonGuard {
                    unless: false,
                    cond: JsonCond::Ne("status".into(), JsonLit::Str("stable".into())),
                }],
            }),
        ))
    );
    // r435 shared-mime-info: unguarded map whose
    // capture carries a `tr` (rides the guarded shape
    // with no guards).
    assert_eq!(
        shape(
            "    strategy :json do |json, regex|\n      json.map { |item| item[\"tag_name\"]&.[](regex, 1)&.tr(\"-\", \".\") }\n    end"
        ),
        Some(JsonShape::MapGuarded(
            vec![],
            Box::new(MapGuard {
                item_key: Some("tag_name".into()),
                capture: MapCapture::BlockRegex,
                tr: Some(("-".into(), ".".into())),
                sub: None,
                guards: vec![],
            }),
        ))
    );
    // Postfix guard: `unless` on the value line skips
    // when the condition holds (`next if` polarity).
    assert_eq!(
        shape(
            "    strategy :json do |json, regex|\n      json.filter_map { |item| item[\"tag_name\"]&.[](regex, 1) unless item[\"upcoming_release\"] }\n    end"
        ),
        Some(JsonShape::MapGuarded(
            vec![],
            Box::new(MapGuard {
                item_key: Some("tag_name".into()),
                capture: MapCapture::BlockRegex,
                tr: None,
                sub: None,
                guards: vec![JsonGuard {
                    unless: false,
                    cond: JsonCond::Truthy("upcoming_release".into()),
                }],
            }),
        ))
    );
    // A computed guard does not reduce.
    assert_eq!(
        shape(
            "    strategy :json do |json|\n      json.map do |item|\n        next if item[\"files\"].none? { |f| f[\"stable\"] }\n        item[\"version\"]\n      end\n    end"
        ),
        None
    );
}

/// r405: `:extract_plist` strategy blocks reduce to one
/// bundle-id property lookup; anything else stays
/// block-only.
#[test]
fn plist_strategy_block_shapes() {
    use super::super::formula::{PlistBlock, PlistProp};
    let block = |body: &str| {
        let text = format!("cask \"x\" do\n  livecheck do\n    url :url\n{body}\n  end\nend\n");
        parse_livecheck_block(&text).and_then(|s| s.plist_block)
    };
    assert_eq!(
        block(
            "    strategy :extract_plist do |items|\n      items[\"com.example.app\"]&.short_version\n    end"
        ),
        Some(PlistBlock {
            has_regex: false,
            bundle_id: "com.example.app".into(),
            prop: PlistProp::Short,
            regex: false,
        })
    );
    assert_eq!(
        block(
            "    strategy :extract_plist do |items, regex|\n      items[\"com.example.app\"]&.version&.[](regex, 1)\n    end"
        ),
        Some(PlistBlock {
            has_regex: true,
            bundle_id: "com.example.app".into(),
            prop: PlistProp::Bundle,
            regex: true,
        })
    );
    // String interpolation does not reduce.
    assert_eq!(
        block(
            "    strategy :extract_plist do |items|\n      item = items[\"com.example.app\"]\n      \"#{item.short_version},#{item.version}\"\n    end"
        ),
        None
    );
}

/// r433: `:git` blocks reduce to data shapes (the core
/// census bodies; teleport's second fetch and
/// macpine's conditional stay block-only).
#[test]
fn git_block_shapes() {
    use super::super::formula::{GitBlock, GitOp};
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url :stable\n    regex(/^v?(\\d+(?:[._-]\\d+)+)$/i)\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.git_block);
    let base = GitBlock {
        needs_spec_regex: false,
        skip_tags: Vec::new(),
        op: GitOp::Tr("_".into(), ".".into()),
        match_template: None,
        skip_split_contains: None,
    };
    // anyzig: filter_map + tr.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      tags.filter_map { |tag| tag[regex, 1]&.tr(\"_\", \".\") }\n    end"
        ),
        Some(base.clone())
    );
    // ace: plain map (the oracle compacts either way).
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      tags.map { |tag| tag[regex, 1]&.tr(\"_\", \".\") }\n    end"
        ),
        Some(base.clone())
    );
    // binkd: `regex` read without declaring it.
    assert_eq!(
        shape(
            "    strategy :git do |tags|\n      tags.filter_map { |tag| tag[regex, 1]&.tr(\"_\", \".\") }\n    end"
        ),
        Some(GitBlock {
            needs_spec_regex: true,
            ..base.clone()
        })
    );
    // fairy-stockfish: match() capture spelling.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      tags.filter_map { |tag| tag.match(regex)&.[](1)&.tr(\"_\", \".\") }\n    end"
        ),
        Some(base.clone())
    );
    // libtrace: regex gsub, empty replacement.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      tags.filter_map { |tag| tag[regex, 1]&.gsub(/-1$/, \"\") }\n    end"
        ),
        Some(GitBlock {
            op: GitOp::GsubRe("-1$".into(), String::new(), String::new()),
            ..base.clone()
        })
    );
    // json_spirit: backreference template.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      tags.filter_map { |tag| tag[regex, 1]&.gsub(/(\\d+)\\.(\\d+)\\.(\\d+)/, '\\1.\\2\\3') }\n    end"
        ),
        Some(GitBlock {
            op: GitOp::GsubRe(
                "(\\d+)\\.(\\d+)\\.(\\d+)".into(),
                String::new(),
                "\\1.\\2\\3".into()
            ),
            ..base.clone()
        })
    );
    // openjdk@8: literal gsub.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      tags.filter_map { |tag| tag[regex, 1]&.gsub(\"8u\", \"1.8.0+\") }\n    end"
        ),
        Some(GitBlock {
            op: GitOp::GsubLit("8u".into(), "1.8.0+".into()),
            ..base.clone()
        })
    );
    // codec2: skip-list guard, identity transform.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      malformed_tags = [\"v1.03\"].freeze\n      tags.map do |tag|\n        next if malformed_tags.include?(tag)\n\n        tag[regex, 1]\n      end\n    end"
        ),
        Some(GitBlock {
            skip_tags: vec!["v1.03".into()],
            op: GitOp::Identity,
            ..base.clone()
        })
    );
    // r472 pxplay: match-assign template over caps.
    {
        use super::super::formula::LatestPart;
        assert_eq!(
            shape(
                "    strategy :git do |tags, regex|\n      tags.filter_map do |tag|\n        match = tag.match(regex)\n        next if match.blank?\n\n        \"#{match[1]},#{match[2]}\"\n      end\n    end"
            ),
            Some(GitBlock {
                op: GitOp::Identity,
                match_template: Some(vec![
                    LatestPart::Cap(1),
                    LatestPart::Lit(",".into()),
                    LatestPart::Cap(2),
                ]),
                ..base.clone()
            })
        );
        // r472 hdfview: split-contains skip guard.
        assert_eq!(
            shape(
                "    strategy :git do |tags, regex|\n      tags.filter_map do |tag|\n        match = tag.match(regex)\n        next if match.blank? || match[1].split(\".\").include?(\"99\")\n\n        match[1]\n      end\n    end"
            ),
            Some(GitBlock {
                op: GitOp::Identity,
                match_template: Some(vec![LatestPart::Cap(1)]),
                skip_split_contains: Some((".".into(), "99".into())),
                ..base.clone()
            })
        );
    }
    // teleport: a second fetch does not reduce.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      data = JSON.parse(Homebrew::Livecheck::Strategy.page_content(\"https://rlz.teleport.sh/versions\")[:content])\n      tagged_versions = tags.filter_map { |tag| tag[regex, 1] }\n      tagged_versions & []\n    end"
        ),
        None
    );
    // macpine: assignment + conditional does not reduce.
    assert_eq!(
        shape(
            "    strategy :git do |tags, regex|\n      tags.map do |tag|\n        version = tag[regex, 1]\n        next if version.blank?\n\n        tag.match?(/^v\\.?\\d+$/i) ? version.chars.join(\".\") : version\n      end\n    end"
        ),
        None
    );
}

/// r434: `:github_latest` blocks reduce to data
/// shapes (lammps' date composite stays block-only).
#[test]
fn github_latest_block_shapes() {
    use super::super::formula::{LatestBlock, LatestOp, LatestPart, LatestRender, LatestSource};
    let spec = |body: &str| {
        let text = format!(
            "class X < Formula\n  livecheck do\n    url :stable\n    regex(/^v?(\\d+(?:[._-]\\d+)+)$/i)\n{body}\n  end\nend\n"
        );
        parse_livecheck_block(&text)
    };
    let shape = |body: &str| spec(body).and_then(|s| s.latest_block);
    let base = LatestBlock {
        needs_spec_regex: false,
        source: LatestSource::TagName,
        scan: false,
        tr: None,
        render: LatestRender::Capture,
        two_b: None,
        pre_capture: None,
        match_pre: false,
        asset_pick: None,
    };
    // expat: scan of tag_name with tr.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"tag_name\"]&.scan(regex)&.map { |match| match[0].tr(\"_\", \".\") }\n    end"
        ),
        Some(LatestBlock {
            scan: true,
            tr: Some(("_".into(), ".".into())),
            ..base.clone()
        })
    );
    // mame: scan of name, no tr.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"name\"]&.scan(regex)&.map { |match| match[0] }\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::Name,
            scan: true,
            ..base.clone()
        })
    );
    // arabica: published_at, deleting tr.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"published_at\"]&.scan(regex)&.map { |match| match[0].tr(\"-\", \"\") }\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::PublishedAt,
            scan: true,
            tr: Some(("-".into(), String::new())),
            ..base.clone()
        })
    );
    // squid: capture subscript with tr.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"tag_name\"]&.[](regex, 1)&.tr(\"_\", \".\")\n    end"
        ),
        Some(LatestBlock {
            tr: Some(("_".into(), ".".into())),
            ..base.clone()
        })
    );
    // vcpkg: match form, plain capture.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      match = json[\"tag_name\"]&.match(regex)\n      next if match.blank?\n\n      match[1]\n    end"
        ),
        Some(base.clone())
    );
    // macvim: body field.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      match = json[\"body\"]&.match(regex)\n      next if match.blank?\n\n      match[1]\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::Body,
            ..base.clone()
        })
    );
    // frege: per-asset name captures.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"assets\"]&.map do |asset|\n        match = asset[\"name\"]&.match(regex)\n        next if match.blank?\n\n        match[1]\n      end\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::AssetName,
            ..base.clone()
        })
    );
    // r453: asset browser_download_url + CSV render.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"assets\"]&.map do |asset|\n        match = asset[\"browser_download_url\"]&.match(regex)\n        next if match.blank?\n\n        \"#{match[2]},#{match[1]}\"\n      end\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::AssetDownloadUrl,
            render: LatestRender::Template(vec![
                LatestPart::Cap(2),
                LatestPart::Lit(",".into()),
                LatestPart::Cap(1),
            ]),
            ..base.clone()
        })
    );
    // r453: early-next equality form.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"assets\"]&.map do |asset|\n        match = asset[\"browser_download_url\"]&.match(regex)\n        next if match.blank?\n\n        next match[1] if match[1] == match[2]\n\n        \"#{match[1]},#{match[2]}\"\n      end\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::AssetDownloadUrl,
            render: LatestRender::EqElse {
                a: LatestOp::Cap(1),
                b: LatestOp::Cap(2),
                then: vec![LatestPart::Cap(1)],
                els: vec![
                    LatestPart::Cap(1),
                    LatestPart::Lit(",".into()),
                    LatestPart::Cap(2),
                ],
            },
            ..base.clone()
        })
    );
    // r453: scan statement form with present? fork.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"tag_name\"]&.scan(regex)&.map do |match|\n        match[1].present? ? \"#{match[0]},#{match[1]}\" : match[0]\n      end\n    end"
        ),
        Some(LatestBlock {
            scan: true,
            render: LatestRender::ScanPresentElse {
                cap: 1,
                then: vec![
                    LatestPart::Cap(0),
                    LatestPart::Lit(",".into()),
                    LatestPart::Cap(1),
                ],
                els: vec![LatestPart::Cap(0)],
            },
            ..base.clone()
        })
    );
    // r453: filter_map direct asset capture.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      json[\"assets\"]&.filter_map do |asset|\n        asset[\"browser_download_url\"]&.[](regex, 1)\n      end\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::AssetDownloadUrl,
            ..base.clone()
        })
    );
    // r453: two-capture name-vs-tag equality.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      version = json[\"name\"]&.[](regex, 1)\n      tag_version = json[\"tag_name\"]&.[](regex, 1)\n      next if version.blank? || tag_version.blank?\n\n      (version == tag_version) ? tag_version : \"#{version},#{tag_version}\"\n    end"
        ),
        Some(LatestBlock {
            source: LatestSource::Name,
            render: LatestRender::TwoEq,
            two_b: Some(LatestSource::TagName),
            ..base.clone()
        })
    );
    // r455: pre-capture ops local + assets EqElse.
    {
        use super::super::formula::{LatestPre, LatestPreOp};
        assert_eq!(
            shape(
                "    strategy :github_latest do |json, regex|\n      tag_version = json[\"tag_name\"]&.tr(\"-\", \".\")&.gsub(/^v/, \"\")\n      next if tag_version.blank?\n\n      json[\"assets\"]&.map do |asset|\n        match = asset[\"name\"]&.match(regex)\n        next if match.blank?\n\n        (match[1] == tag_version) ? tag_version : \"#{tag_version},#{match[1]}\"\n      end\n    end"
            ),
            Some(LatestBlock {
                source: LatestSource::AssetName,
                pre_capture: Some(LatestPre {
                    source: LatestSource::TagName,
                    lit: None,
                    capture: false,
                    ops: vec![
                        LatestPreOp::Tr("-".into(), ".".into()),
                        LatestPreOp::DeletePrefix("v".into()),
                    ],
                }),
                render: LatestRender::EqElse {
                    a: LatestOp::Cap(1),
                    b: LatestOp::Local,
                    then: vec![LatestPart::Local],
                    els: vec![
                        LatestPart::Local,
                        LatestPart::Lit(",".into()),
                        LatestPart::Cap(1),
                    ],
                },
                ..base.clone()
            })
        );
        // r455: `next unless match` blank guard spelling.
        assert_eq!(
            shape(
                "    strategy :github_latest do |json, regex|\n      match = json[\"tag_name\"]&.match(regex)\n      next unless match\n\n      match[2].present? ? \"#{match[1]},#{match[2]}\" : match[1]\n    end"
            ),
            Some(LatestBlock {
                render: LatestRender::PresentElse {
                    cap: 2,
                    then: vec![
                        LatestPart::Cap(1),
                        LatestPart::Lit(",".into()),
                        LatestPart::Cap(2),
                    ],
                    els: vec![LatestPart::Cap(1)],
                },
                ..base.clone()
            })
        );
    }
    // lammps: date composite does not reduce.
    assert_eq!(
        shape(
            "    strategy :github_latest do |json, regex|\n      match = json[\"tag_name\"]&.match(regex)\n      next if match.blank? || match[1].blank?\n\n      date_str = Date.parse(match[1]).strftime(\"%Y%m%d\")\n      match[2].present? ? \"#{date_str}-#{match[2]}\" : date_str\n    end"
        ),
        None
    );
}

/// r434: the agreement gate's spec version is the
/// class/`stable do` stanza only; `fails_with` floors
/// and resource versions are not it.
#[test]
fn spec_version_stanza_scopes() {
    // Class level.
    assert_eq!(
        spec_version_stanza("class X < Formula\n  version \"1.2.3\"\nend\n"),
        Some("1.2.3".to_string())
    );
    // Inside `stable do`.
    assert_eq!(
        spec_version_stanza(
            "class X < Formula\n  stable do\n    url \"https://example.org/x.tgz\"\n    version \"4.5\"\n  end\nend\n"
        ),
        Some("4.5".to_string())
    );
    // micromamba: a `fails_with` floor is not it.
    assert_eq!(
        spec_version_stanza(
            "class X < Formula\n  url \"https://example.org/x-2.10.0.tgz\"\n\n  fails_with :gcc do\n    version \"13\"\n    cause \"x\"\n  end\nend\n"
        ),
        None
    );
    // tcl-tk: a resource version is not it.
    assert_eq!(
        spec_version_stanza(
            "class X < Formula\n  resource \"itk4\" do\n    url \"https://example.org/itk.tgz\"\n    version \"4.2.3\"\n  end\nend\n"
        ),
        None
    );
    // A resource nested in `stable do` is not it either.
    assert_eq!(
        spec_version_stanza(
            "class X < Formula\n  stable do\n    resource \"r\" do\n      version \"9.9\"\n    end\n    version \"1.0\"\n  end\nend\n"
        ),
        Some("1.0".to_string())
    );
}

/// r441: a git stable spec's version detects from
/// its `tag:` (k9s: the URL's own digits read "9").
#[test]
fn spec_git_tag_reads_stable_url_statement() {
    let text = "class K9s < Formula\n  url \"https://github.com/derailed/k9s.git\",\n      tag:      \"v0.51.0\",\n      revision: \"558caafe\"\nend\n";
    assert_eq!(
        spec_git_tag(text, "https://github.com/derailed/k9s.git").as_deref(),
        Some("v0.51.0")
    );
    // Same-line form.
    let text = "class X < Formula\n  url \"https://example.com/x.git\", tag: \"bun-v1.4.4\"\nend\n";
    assert_eq!(
        spec_git_tag(text, "https://example.com/x.git").as_deref(),
        Some("bun-v1.4.4")
    );
    // A resource's tag is not the stable spec's.
    let text = "class X < Formula\n  url \"https://example.com/x-1.0.tgz\"\n  resource \"r\" do\n    url \"https://example.com/r.git\", tag: \"v2\"\n  end\nend\n";
    assert_eq!(spec_git_tag(text, "https://example.com/x-1.0.tgz"), None);
}

/// r441: `#{Regexp.escape(version.major_minor)}` in
/// a regex stanza defers to plan time via the
/// sentinel (php@8.x); other version tokens in a
/// regex still refuse the spec.
#[test]
fn regex_major_minor_sentinel() {
    use super::super::formula::MAJOR_MINOR_SENTINEL;
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/downloads\"\n    regex(/href=.*?php[._-]v?(#{Regexp.escape(version.major_minor)}(?:\\.\\d+)*)\\.t/i)\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.regex.as_deref(),
        Some(format!("href=.*?php[._-]v?({MAJOR_MINOR_SENTINEL}(?:\\.\\d+)*)\\.t").as_str())
    );
    // r444: plain version chains in a regex also
    // defer to plan time, stored verbatim.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\"\n    regex(/v(#{version.major}\\d+)/)\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.regex.as_deref(), Some(r"v(#{version.major}\d+)"));
}

/// r442: a trailing comment after the regex
/// statement (robotfindskitten) and a regex literal
/// spanning lines (neo4j) both parse.
#[test]
fn livecheck_regex_comment_and_multiline() {
    let spec = parse_livecheck_block(
        "  livecheck do\n    url :stable\n    regex(/^(\\d+(?:\\.\\d+)+)$/i) # Exclude tags that start with `v`\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.regex.as_deref(), Some(r"^(\d+(?:\.\d+)+)$"));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/\"\n    regex(/a(\\d+)\n          |b(\\d+)/ix)\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.regex_flags, "ix");
    assert!(spec.regex.unwrap().contains("|b"));
}

/// r442: the host's platform-conditional stable URL
/// (bic's `on_macos`/`on_arm` archive override).
#[test]
fn spec_platform_url_picks_host_override() {
    let text = "class Bic < Formula\n  stable do\n    url \"https://example.com/releases/bic-1.0.0.tar.gz\"\n    on_macos do\n      on_arm do\n        url \"https://example.com/archive/v1.0.0.tar.gz\"\n      end\n    end\n    on_linux do\n      url \"https://example.com/linux.tar.gz\"\n    end\n  end\nend\n";
    let expected = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("https://example.com/archive/v1.0.0.tar.gz")
    } else if cfg!(target_os = "linux") {
        Some("https://example.com/linux.tar.gz")
    } else {
        None
    };
    assert_eq!(spec_platform_url(text).as_deref(), expected);
    // No conditional anywhere: nothing to override.
    assert_eq!(
        spec_platform_url("  url \"https://example.com/x.tgz\"\n"),
        None
    );
}

/// r443: `:sparkle` blocks reduce to data —
/// block-pass symbols, channel finds, match
/// templates, item field indexes.
#[test]
fn sparkle_block_shapes_parse() {
    use super::super::formula::{SpChannel, SpExpr, SpField, SpPart};
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    strategy :sparkle, &:short_version\n  end\n",
    )
    .unwrap();
    let block = spec.sparkle_block.unwrap();
    assert!(!block.all && !block.needs_spec_regex);
    assert_eq!(block.expr, SpExpr::Field(SpField::Short));

    // r473 macdown: own-version early return.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    strategy :sparkle do |item|\n      next version if item.short_version == \"0.7.3\"\n      item.nice_version\n    end\n  end\n",
    )
    .unwrap();
    let block = spec.sparkle_block.unwrap();
    assert_eq!(block.own_if, Some((SpField::Short, "0.7.3".to_string())));
    // r473 opencat: count-padded local in a template.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    strategy :sparkle do |item|\n      short_version = (item.short_version.count(\".\") >= 2) ? item.short_version : \"#{item.short_version}.0\"\n      \"#{short_version},#{item.version}\"\n    end\n  end\n",
    )
    .unwrap();
    let block = spec.sparkle_block.unwrap();
    assert!(matches!(
        block.expr,
        SpExpr::Template { ref parts, .. }
            if matches!(parts[0], SpPart::CountPad { .. })
    ));
    // r473 macwhisper: cap-blank early return.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/v(\\d+)/)\n    strategy :sparkle do |items, regex|\n      items.map do |item|\n        version_suffix = item.url.match(regex)\n        next item.nice_version if version_suffix.nil? || version_suffix[1].blank?\n        \"#{item.nice_version}_#{version_suffix[1]}\"\n      end\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.sparkle_block.map(|b| b.expr),
        Some(SpExpr::CapBlankElse { .. })
    ));
    // r473 longplay: assign-find under another name.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/v(\\d+)/)\n    strategy :sparkle do |items|\n      stable_item = items.find { |item| item.channel.nil? }\n      next unless stable_item\n      stable_item.url&.[](regex, 1)&.tr(\"-\", \",\")\n    end\n  end\n",
    )
    .unwrap();
    let block = spec.sparkle_block.unwrap();
    assert!(block.find);
    assert!(matches!(block.expr, SpExpr::FieldIndexTr(..)));
    // r473 tripmode: scan if/else.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/v(\\h+)?-(\\d+)/)\n    strategy :sparkle do |item, regex|\n      item.url.scan(regex).map do |match|\n        if match[1] == item.version\n          item.nice_version\n        else\n          \"#{item.nice_version},#{match[1]}\"\n        end\n      end\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.sparkle_block.map(|b| b.expr),
        Some(SpExpr::ScanEqElse { .. })
    ));

    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    strategy :sparkle do |items|\n      items.find { |item| item.channel.nil? }&.nice_version\n    end\n  end\n",
    )
    .unwrap();
    let block = spec.sparkle_block.unwrap();
    assert_eq!(block.channel, Some(SpChannel::Nil));
    assert_eq!(block.expr, SpExpr::Field(SpField::Nice));

    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/v(\\d+)-(\\d+)/)\n    strategy :sparkle do |item, regex|\n      match = item.url.match(regex)\n      next if match.blank?\n      \"#{item.short_version},#{match[2]},#{match[1]}\"\n    end\n  end\n",
    )
    .unwrap();
    let block = spec.sparkle_block.unwrap();
    assert!(block.needs_spec_regex);
    assert_eq!(
        block.expr,
        SpExpr::Template {
            from: Some(SpField::Url),
            local: None,
            parts: vec![
                SpPart::Field(SpField::Short),
                SpPart::Lit(",".to_string()),
                SpPart::Cap(2),
                SpPart::Lit(",".to_string()),
                SpPart::Cap(1),
            ],
        }
    );

    // r456: EqElse render, assign-find with present
    // guards, chars.join.
    {
        use super::super::formula::SpOp;
        let spec = parse_livecheck_block(
            "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/v(\\d+)-(\\d+)/)\n    strategy :sparkle do |item, regex|\n      match = item.url&.match(regex)\n      next if match.blank?\n\n      (match[1] == match[2]) ? match[1] : \"#{match[1]},#{match[2]}\"\n    end\n  end\n",
        )
        .unwrap();
        let block = spec.sparkle_block.unwrap();
        assert_eq!(
            block.expr,
            SpExpr::EqElse {
                from: Some(SpField::Url),
                local: None,
                a: SpOp::Cap(1),
                b: SpOp::Cap(2),
                then: vec![SpPart::Cap(1)],
                els: vec![SpPart::Cap(1), SpPart::Lit(",".to_string()), SpPart::Cap(2),],
            }
        );
        let spec = parse_livecheck_block(
            "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/x/)\n    strategy :sparkle do |items, regex|\n      item = items.find { |item| item.channel.nil? && item.url&.match(regex) }\n      next if item&.version.nil? || item.short_version.nil?\n\n      \"#{item.version},#{item.short_version}\"\n    end\n  end\n",
        )
        .unwrap();
        let block = spec.sparkle_block.unwrap();
        assert_eq!(block.channel, Some(SpChannel::Nil));
        assert_eq!(block.match_guard, Some(SpField::Url));
        assert_eq!(block.present, vec![SpField::Full, SpField::Short]);
        let spec = parse_livecheck_block(
            "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    strategy :sparkle do |item|\n      item.version.chars.join(\".\")\n    end\n  end\n",
        )
        .unwrap();
        assert_eq!(
            spec.sparkle_block.unwrap().expr,
            SpExpr::FieldCharsJoin(SpField::Full, ".".to_string())
        );
        // `compressed: false` is a modeled no-op.
        let spec = parse_livecheck_block(
            "  livecheck do\n    url \"https://example.com/appcast.xml\",\n        compressed: false\n    strategy :sparkle, &:short_version\n  end\n",
        )
        .unwrap();
        assert!(!spec.url_options && spec.sparkle_block.is_some());
    }

    // An unguarded match stays block-only.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/v(\\d+)/)\n    strategy :sparkle do |item, regex|\n      match = item.url.match(regex)\n      match[1]\n    end\n  end\n",
    )
    .unwrap();
    assert!(spec.sparkle_block.is_none());
}

fn sparkle_fixture(params: &str, body: &str) -> Option<super::super::formula::SparkleBlock> {
    parse_livecheck_block(&format!(
        "  livecheck do\n    url \"https://example.com/appcast.xml\"\n    regex(/v(\\d+-\\d+)/)\n    strategy :sparkle do |{params}|\n{body}\n    end\n  end\n"
    ))?
    .sparkle_block
}

#[test]
fn sparkle_field_indexes_keep_bare_and_transformed_forms() {
    use super::super::formula::{SpExpr, SpField};
    for index in ["[regex, 1]", ".[](regex, 1)", "&.[](regex, 1)"] {
        let block = sparkle_fixture("item, regex", &format!("      item.url{index}")).unwrap();
        assert_eq!(block.expr, SpExpr::FieldIndex(SpField::Url, 1), "{index}");
        assert!(block.needs_spec_regex);
        for transform in [".tr(\"-\", \",\")", "&.tr(\"-\", \",\")"] {
            let block =
                sparkle_fixture("item, regex", &format!("      item.url{index}{transform}"))
                    .unwrap();
            assert_eq!(
                block.expr,
                SpExpr::FieldIndexTr(SpField::Url, 1, "-".into(), ",".into()),
                "{index}{transform}"
            );
        }
    }
    for invalid in [
        "[regex, 1)",
        ".[](regex, 1]",
        "&.[](regex, 1]",
        "[regex, -1]",
        "[regex, 1].unknown",
        "[regex, 1]more",
    ] {
        assert!(
            sparkle_fixture("item, regex", &format!("      item.url{invalid}")).is_none(),
            "unsupported index tail: {invalid}"
        );
    }
}

#[test]
fn sparkle_assign_find_parses_its_receiver_without_changing_literal_text() {
    use super::super::formula::{SpChannel, SpExpr, SpField, SpPart};
    let selected = |body: &str| {
        sparkle_fixture(
            "items, regex",
            &format!(
                "      stable_item = items.find {{ |item| item.channel.nil? }}\n      next unless stable_item\n{body}"
            ),
        )
    };
    let block =
        selected(r##"      "#{stable_item.nice_version}-stable_item.url-other_stable_item.url""##)
            .unwrap();
    assert!(block.find);
    assert_eq!(block.channel, Some(SpChannel::Nil));
    assert_eq!(
        block.expr,
        SpExpr::Template {
            from: None,
            local: None,
            parts: vec![
                SpPart::Field(SpField::Nice),
                SpPart::Lit("-stable_item.url-other_stable_item.url".into()),
            ],
        }
    );
    let block = selected("      stable_item.url[regex, 1]").unwrap();
    assert_eq!(block.expr, SpExpr::FieldIndex(SpField::Url, 1));
    for receiver in ["item", "stable_item"] {
        for presence in [
            format!("next unless {receiver}"),
            format!("next if {receiver}.nil?"),
        ] {
            let block = sparkle_fixture(
                "items, regex",
                &format!(
                    "      {receiver} = items.find {{ |item| item.url.match(regex) }}\n      {presence}\n      {receiver}.nice_version"
                ),
            )
            .unwrap();
            assert!(block.find);
            assert_eq!(block.match_guard, Some(SpField::Url));
            assert!(block.needs_spec_regex);
            assert_eq!(block.expr, SpExpr::Field(SpField::Nice));
        }
    }
    let block = selected(
        "      next if stable_item&.short_version.blank?\n      found = stable_item.url.match(regex)\n      next if found.blank?\n      \"#{stable_item.nice_version}-#{found[1]}\"",
    )
    .unwrap();
    assert_eq!(block.present, [SpField::Short]);
    assert_eq!(
        block.expr,
        SpExpr::Template {
            from: Some(SpField::Url),
            local: None,
            parts: vec![
                SpPart::Field(SpField::Nice),
                SpPart::Lit("-".into()),
                SpPart::Cap(1),
            ],
        }
    );
    for wrong in [
        "stable_item_extra.url",
        "other_stable_item.url",
        "object.stable_item.url",
        "item.url",
        r##""#{stable_item_extra.version}""##,
        r##""#{other_stable_item.version}""##,
    ] {
        assert!(selected(wrong).is_none(), "another receiver: {wrong}");
    }
}

#[test]
fn sparkle_templates_distinguish_escaped_interpolation_from_receiver_reads() {
    use super::super::formula::{SpExpr, SpField, SpPart};
    for (template, parts) in [
        (
            r##""\#{stable_item.version}|#{stable_item.nice_version}""##,
            vec![
                SpPart::Lit("#{stable_item.version}|".into()),
                SpPart::Field(SpField::Nice),
            ],
        ),
        (
            r##""\\#{stable_item.nice_version}""##,
            vec![SpPart::Lit("\\".into()), SpPart::Field(SpField::Nice)],
        ),
        (
            r##""\\\#{other_stable_item.version}|#{stable_item.nice_version}""##,
            vec![
                SpPart::Lit("\\#{other_stable_item.version}|".into()),
                SpPart::Field(SpField::Nice),
            ],
        ),
        (
            r##""stable_item.url \"#{stable_item.nice_version}\"""##,
            vec![
                SpPart::Lit("stable_item.url \"".into()),
                SpPart::Field(SpField::Nice),
                SpPart::Lit("\"".into()),
            ],
        ),
        (
            r##""\##{stable_item.nice_version}""##,
            vec![SpPart::Lit("#".into()), SpPart::Field(SpField::Nice)],
        ),
        (
            r##""#{stable_item.nice_version}\#""##,
            vec![SpPart::Field(SpField::Nice), SpPart::Lit("#".into())],
        ),
    ] {
        let body = format!(
            "      stable_item = items.find {{ |item| item.channel.nil? }}\n      next unless stable_item\n      {template}"
        );
        assert_eq!(
            sparkle_fixture("items, regex", &body).unwrap().expr,
            SpExpr::Template {
                from: None,
                local: None,
                parts,
            },
            "{template}"
        );
    }
    assert!(
        sparkle_fixture("item", r##"      "#{item.nice_version""##).is_none(),
        "an unfinished expression is not a literal"
    );
}

/// r445: statement-form `:json` bodies (locals,
/// guarded match, CSV templates) reduce to a
/// `JsonTemplate`; interpolation-aware strings.
#[test]
fn json_template_shapes_parse() {
    use super::super::formula::{JsonLocal, JsonPart, JsonRender, JsonShape};
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)-(\\d+)/)\n    strategy :json do |json, regex|\n      match = json[\"url\"]&.match(regex)\n      next if match.blank?\n      \"#{match[2]},#{match[1]}\"\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(!tmpl.map);
    assert_eq!(
        tmpl.render,
        JsonRender::Template(vec![
            JsonPart::Cap(2),
            JsonPart::Lit(",".to_string()),
            JsonPart::Cap(1),
        ])
    );
    // Two-local CSV template over a map.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"releases\"]&.map do |release|\n        version = release[\"version\"]\n        build = release[\"build\"]\n        next if version.blank? || build.blank?\n        \"#{version},#{build}\"\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(tmpl.map && tmpl.locals.len() == 2 && tmpl.require.len() == 2);
    assert!(matches!(tmpl.locals[0], JsonLocal::Dig(_)));
    // `#{...}` containing quotes keeps the string.
    let (tok, tail) = quoted_token("\"https://x/?#{DateTime.now.strftime(\"%Q\")}\"").unwrap();
    assert_eq!(tok, "https://x/?#{DateTime.now.strftime(\"%Q\")}");
    assert!(tail.is_empty());
    // r458: find-locals, int/field-blank guards,
    // match-block, cap forks.
    use super::super::formula::{JsonCond, JsonFindCond, JsonLit, JsonTail, JsonValue};
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      stable = json[\"channels\"]&.find { |channel| channel[\"id\"] == \"stable\" }\n      next unless stable\n      stable[\"version\"]\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert_eq!(
        tmpl.finds[0].cond,
        JsonFindCond::FieldEq("id".into(), JsonLit::Str("stable".into()))
    );
    assert_eq!(tmpl.require_find, vec![(0, false)]);
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"data\"]&.map do |item|\n        next if item[\"is_beta\"] == 1\n        next if item[\"dl\"].blank?\n        item[\"version\"]&.strip\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert_eq!(
        tmpl.guards[0].cond,
        JsonCond::Eq("is_beta".into(), JsonLit::Int(1))
    );
    assert_eq!(tmpl.guards[1].cond, JsonCond::FieldBlank("dl".into()));
    assert!(matches!(
        tmpl.render,
        JsonRender::Value(JsonValue::Local(_), JsonTail::Strip)
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)(?:-(\\d+))?/)\n    strategy :json do |json, regex|\n      match = json[\"version\"]&.match(regex)\n      next unless match\n      match[2].present? ? \"#{match[1]},#{match[2]}\" : match[1]\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(
        tmpl.render,
        JsonRender::CapFork {
            cap: 2,
            present: true,
            ..
        }
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)/)\n    strategy :json do |json, regex|\n      json[\"name\"]&.match(regex) { |match| match[1] }\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::DigRegex(_))
    ));
    // r463: dig-blank guards, object locals,
    // scan/captures shapes.
    use super::super::formula::JsonCond as JC;
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json.map do |item|\n        next if item.dig(\"osx\", \"server\").blank?\n        next if !item[\"a\"]\n        item[\"version\"]\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(tmpl.guards[0].cond, JC::DigBlank(ref p) if p.len() == 2));
    assert!(matches!(tmpl.guards[1].cond, JC::Truthy(ref f) if f == "a") && tmpl.guards[1].unless);
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      v = json.dig(\"cura\", \"Darwin\")\n      \"#{v[\"major\"]}.#{v[\"minor\"]}\"\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(tmpl.render, JsonRender::Template(_)) && tmpl.locals.len() == 3);
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)-(\\d+)/)\n    strategy :json do |json, regex|\n      json.dig(\"data\", \"url\")&.scan(regex)&.map { |match| \"#{match[1]},#{match[0]}\" }\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::DigScan(_, _))
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)/)\n    strategy :json do |json, regex|\n      json[\"url\"]&.match(regex)&.captures\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::DigCaptures(_))
    ));
    // r466: object-values maps, plain sub, postfix
    // render guards, literal-regex guards.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"versionTable\"].map { |_, entry| entry[\"originVersion\"] }\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::ValuesKey(_, ref k)) if k == "originVersion"
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"version\"].to_s.sub(/-?b/, \",\")\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::DigSubPlain(_, _, _, ref r)) if r == ","
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"value\"]&.map do |build|\n        next if build[\"desc\"].match?(/beta/i)\n        build[\"id\"]&.to_s if build[\"sourceBranch\"] == \"refs/heads/stable\"\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(tmpl.guards[0].cond, JC::MatchRe(..)));
    assert!(tmpl.guards[1].unless);
    // r469 alfaview: mid-path `.first` and the
    // bracket-call form dig as path segments.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"versions\"]&.first&.[](\"version\")\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::Dig(_))
    ));
    // r469 onekey: an array join tail.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json.dig(\"desktop\", \"version\")&.join(\".\")\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::DigJoin(_, ref s)) if s == "."
    ));
    // r469 burp: a map over a dug local, postfix
    // `&&` guards across lines, an `any?` element
    // block, and a cask-local `.to_s`.
    let spec = parse_livecheck_block(
        "cask \"x\" do\n  arch arm: \"MacOsArm64\", intel: \"MacOsx\"\n  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      all_versions = json.dig(\"ResultSet\", \"Results\")\n      next if all_versions.blank?\n      all_versions.filter_map do |item|\n        item[\"version\"] if\n              item[\"releaseChannels\"]&.include?(\"Stable\") &&\n              item[\"builds\"]&.any? do |build|\n                build[\"BuildCategoryId\"] == \"desktop\" &&\n                build[\"BuildCategoryPlatform\"] == arch.to_s\n              end\n      end\n    end\n  end\nend\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(tmpl.map);
    assert_eq!(tmpl.guards.len(), 2);
    assert!(matches!(tmpl.guards[1].cond, JC::AnyEq(..)));
    // r469 vscode: a tab before `=` still assigns.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      version = json[\"productVersion\"]\n      timestamp\t= json[\"timestamp\"]\n      next if version.blank? || timestamp.blank?\n      \"#{version},#{timestamp}\"\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::Template(_))
    ));
    // r469 soundtoys: an entries map with a key
    // regex guard.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json.map do |key, item|\n        next unless key.match?(/Mac/i)\n        item[\"fullversion\"]\n      end\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::EntriesMap(_, _))
    ));
    // r469 pixcake: literal-array membership and a
    // trailing `&.max`.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"data\"]&.filter_map do |item|\n        next unless [15, 17].include?(item[\"app_type\"])\n        item[\"version\"]\n      end&.max\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::MapGuardedMax(_, guard)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected max shape");
    };
    assert!(matches!(guard.guards[0].cond, JC::FieldIn(..)));
    // r471 warp: (dig || dig)&.delete_prefix.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      (json.dig(\"stable\", \"tui_version\") || json.dig(\"stable\", \"version\"))&.delete_prefix(\"v\")\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::Template(ref t))
            if matches!(t.locals[0], JsonLocal::DigOrTail(..))
    ));
    // r471 yealink: inline digs in a root template.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      \"#{json.dig(\"data\", \"packageVersion\")},#{json.dig(\"data\", \"md5\")}\"\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::Template(_))
    ));
    // r471 proxybridge: local-equality fork.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)/)\n    strategy :json do |json|\n      ver = json.dig(\"macos\", \"version\")\n      next unless ver\n      tag = json.dig(\"macos\", \"download\")&.[](regex, 1)\n      next unless tag\n      (tag == ver) ? ver : \"#{ver},#{tag}\"\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(tmpl.render, JsonRender::LocalEqFork { .. }));
    // r471 dolphin: a root dig inside a map template.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)-(\\h+)/)\n    strategy :json do |json, regex|\n      json[\"artifacts\"]&.map do |artifact|\n        match = artifact[\"url\"]&.match(regex)\n        next if match.blank?\n        \"#{json[\"shortrev\"]},#{match[1]},#{match[2]}\"\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(
        tmpl.render,
        JsonRender::Template(ref parts) if matches!(parts[0], JsonPart::RootDig(_))
    ));
    // r471 leigod: values map + parenthesized match guard.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)/)\n    strategy :json do |json, regex|\n      json[\"mac\"]&.values&.filter_map do |item|\n        next unless (match = item[\"download_url\"]&.match(regex))\n        match[1]\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(tmpl.values && tmpl.map);
    // r471 pcsx2: a dig-truthy guard.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)/)\n    strategy :json do |json, regex|\n      json[\"data\"]&.map do |release|\n        next unless release.dig(\"assets\", \"MacOS\")\n        release[\"version\"]&.[](regex, 1)\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(tmpl.guards[0].cond, JC::DigTruthy(_)));
    // r471 mongrel: an if-block wrapper as guards.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json[\"files\"]&.filter_map do |file|\n        if file[\"platform\"] == \"macos\" && file[\"file_name\"]&.end_with?(\".dmg\")\n          \"#{file[\"version\"]},#{file[\"id\"]}\"\n        end\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(tmpl.guards[1].cond, JC::EndsWith(..)));
    // r471 zoho: one-line brace values map.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    regex(/v(\\d+)/)\n    strategy :json do |json, regex|\n      json[\"mac\"]&.values&.filter_map { |item| item[\"arm64\"]&.[](regex, 1) }\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(tmpl.values);
    // r471 julia: entry keys passing value guards.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.json\"\n    strategy :json do |json|\n      json.map do |version, release|\n        next unless release[\"stable\"]\n        next unless release[\"files\"].any? { |file| file[\"os\"] == \"mac\" && file[\"arch\"] == \"aarch64\" }\n        version\n      end\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::EntriesKeys(_, _))
    ));
}

/// r446: cask url interpolation with a conditional
/// quoted segment (`#{"-#{...}" if ...}`) and the
/// `token` local. `arch` is this host's: `mac_arm` on
/// arm64, `mac` on x86_64.
#[test]
fn cask_url_conditional_interpolation() {
    let text = "cask \"demo\" do\n  arch arm: \"mac_arm\", intel: \"mac\"\n  version \"2026.3.1.1,stingray1\"\n  url \"https://example.com/#{version.csv.first}/app#{\"-#{version.csv.second}\" if version.csv.second}-#{arch}.dmg\"\nend\n";
    let c = parse_cask_rb_as_data(text, "demo");
    let arch = if cfg!(target_arch = "x86_64") {
        "mac"
    } else {
        "mac_arm"
    };
    assert_eq!(
        c.url,
        format!("https://example.com/2026.3.1.1/app-stingray1-{arch}.dmg")
    );
    let text = "cask \"demo\" do\n  version \"4,2023.07\"\n  url \"https://example.com/#{version.csv.second.major}/#{token}#{version.major}.zip\"\nend\n";
    let c = parse_cask_rb_as_data(text, "demo");
    assert_eq!(c.url, "https://example.com/2023/demo4.zip");
    // r471 julia: a string-op chain on a cask local (`arch` this host's).
    let text = "cask \"demo\" do\n  arch arm: \"aarch64\", intel: \"x64\"\n  version \"1.13.4\"\n  url \"https://example.com/mac#{arch.delete_prefix(\"x\")}.dmg\"\nend\n";
    let c = parse_cask_rb_as_data(text, "demo");
    let arch = if cfg!(target_arch = "x86_64") {
        "64"
    } else {
        "aarch64"
    };
    assert_eq!(c.url, format!("https://example.com/mac{arch}.dmg"));
}

/// r447: `:xml` template bodies reduce (xpath
/// predicates, locals, map projections).
#[test]
fn xml_template_shapes_parse() {
    use super::super::formula::{XmlPart, XmlPred, XmlProj, XmlRender, XmlShape};
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    strategy :xml do |xml|\n      version = xml.elements[\"//dict/key[text()='version']\"]&.next_element&.text\n      build = xml.elements[\"//dict/key[text()='build']\"]&.next_element&.text\n      next if version.blank? || build.blank?\n      \"#{version.strip},#{build.strip}\"\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert_eq!(tmpl.locals.len(), 2);
    assert_eq!(
        tmpl.locals[0].path.steps[1].pred,
        Some(XmlPred::TextEq("version".into()))
    );
    assert_eq!(
        tmpl.render,
        XmlRender::Template(vec![
            XmlPart::LocalStrip(0),
            XmlPart::Lit(",".into()),
            XmlPart::LocalStrip(1),
        ])
    );
    // get_elements map with next-sibling text.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    strategy :xml do |xml|\n      xml.get_elements(\"//dict/key[text()='Version']\").map { |item| item.next_element&.text&.strip }\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert_eq!(tmpl.map.map(|m| m.proj), Some(XmlProj::NextText));
    // r457: single-element modifier-if match.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    regex(/v(\\d+)/)\n    strategy :xml do |xml, regex|\n      url = xml.elements[\"//FilePath\"]&.text&.strip\n      match = url.match(regex) if url\n      next if match.blank?\n      match[1]\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert_eq!(tmpl.render, XmlRender::CapValue(1));
    // r457: element local + following-sibling predicate.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.plist\"\n    regex(/(\\d+)/)\n    strategy :xml do |xml, regex|\n      item = xml.elements[\"//dict[key[text()='AppName']/following-sibling::*[1][text()='Demo']]\"]\n      next unless item\n      version = item.elements[\"key[text()='Version']\"]&.next_element&.text\n      next if version.blank?\n      version.strip\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert_eq!(tmpl.require_elems, vec![0]);
    assert_eq!(
        tmpl.elems[0].path.steps[0].pred,
        Some(XmlPred::SiblingAfterChild {
            child: "key".into(),
            child_text: "AppName".into(),
            text: "Demo".into(),
        })
    );
    assert_eq!(tmpl.locals[0].from, Some(0));
    assert!(tmpl.locals[0].path.relative);
    // r465: continued chains, zip maps, relative
    // paths, delete_suffix, present forks.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    strategy :xml do |xml|\n      xml.get_elements(\"//Version\")\n         .map { |item| item.text&.strip&.delete_suffix(\"/\") }\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert!(matches!(
        tmpl.map.map(|m| m.proj),
        Some(super::super::formula::XmlProj::TextDeleteSuffix(s)) if s == "/"
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.plist\"\n    strategy :xml do |xml|\n      versions = xml.get_elements(\"//key[text()='shortVersion']\").map { |item| item.next_element&.text&.strip }\n      builds = xml.get_elements(\"//key[text()='version']\").map { |item| item.next_element&.text&.strip }\n      versions.zip(builds).map do |version, build|\n        \"#{version},#{build}\"\n      end\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.xml_block.map(|b| b.shape),
        Some(XmlShape::ZipPair { .. })
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    regex(/^v?(\\d+)(?:\\.(\\d{3,}))?$/)\n    strategy :xml do |xml, regex|\n      xml.get_elements(\"//full-version\").map do |item|\n        match = item.text&.strip&.match(regex)\n        next if match.blank?\n        match[2].present? ? \"#{match[1]},#{match[2]}\" : match[1]\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert!(matches!(
        tmpl.render,
        super::super::formula::XmlRender::CapPresentFork { cap: 2, .. }
    ));
    // Interpolated predicate values stay outside.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.plist\"\n    strategy :xml do |xml|\n      item = xml.elements[\"//dict[key[text()='App#{version.major}']]\"]\n      next unless item\n      item.elements[\"key[text()='Version']\"]&.next_element&.text\n    end\n  end\n",
    );
    assert!(spec.is_none() || spec.unwrap().xml_block.is_none());
    // r470 qnap: nested child-text predicates, an
    // elem blank guard, and relative text reads.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    strategy :xml do |xml|\n      item = xml.elements[\"//application[productName[text()='Qsync']]/platform[platformName[text()='Mac']]/software\"]\n      next if item.blank?\n      version = item.elements[\"version\"]&.text&.strip\n      build = item.elements[\"buildNumber\"]&.text&.strip\n      next if version.blank? || build.blank?\n      \"#{version},#{build}\"\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert_eq!(tmpl.require_elems, vec![0]);
    assert_eq!(
        tmpl.elems[0].path.steps[0].pred,
        Some(XmlPred::ChildTextEq("productName".into(), "Qsync".into()))
    );
    // r470 zen: a bare attribute template map.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    strategy :xml do |xml|\n      xml.get_elements(\"//update\").map { |item| \"#{item.attributes[\"appVersion\"]},#{item.attributes[\"buildID\"]}\" }\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert_eq!(tmpl.map.map(|m| m.locals.len()), Some(2));
    // r470 emacs: an attribute-fed match in a map.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    regex(/v(\\d+)-(\\h+)/i)\n    strategy :xml do |xml, regex|\n      xml.get_elements(\"/feed/entry/link\").filter_map do |item|\n        match = item.attributes[\"href\"]&.match(regex)\n        next unless match\n        \"#{match[1]},#{match[2]}\"\n      end\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert!(matches!(
        tmpl.map.map(|m| m.proj),
        Some(XmlProj::AttrMatch(ref a)) if a == "href"
    ));
    // r473 yandex: a capture with a tr transform in
    // a template.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    regex(/Yandex_(\\d+(?:\\.\\d+)*)_(\\d+)\\.dmg/i)\n    strategy :xml do |xml, regex|\n      url = xml.elements[\"//item/guidArm64\"]&.text&.strip\n      match = url.match(regex) if url\n      next if match.blank?\n      \"#{match[1].tr(\"_\", \".\")},#{match[2]}\"\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert!(matches!(
        tmpl.render,
        super::super::formula::XmlRender::Template(ref parts)
            if matches!(parts[0], XmlPart::CapTr(1, ..))
    ));
    // r473 lingon: REXML::XPath.first + next_element read.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    strategy :xml do |xml|\n      key = REXML::XPath.first(xml, \"//dict/key[text()='version']\")\n      key.next_element.text\n    end\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.xml_block.map(|b| b.shape),
        Some(XmlShape::Template(_))
    ));
    // r473 macx: or-assign fallback between two reads.
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/x.xml\"\n    strategy :xml do |xml|\n      version = xml.elements[\"//dict/key[text()='LatestVersion']\"]&.next_element&.text\n      version ||= xml.elements[\"//dict/key[text()='LastestVersion']\"]&.next_element&.text\n      next if version.blank?\n      version.strip\n    end\n  end\n",
    )
    .unwrap();
    let Some(XmlShape::Template(tmpl)) = spec.xml_block.map(|b| b.shape) else {
        panic!("expected xml template");
    };
    assert_eq!(tmpl.or_pairs, vec![(0, 1)]);
}

/// r448: `post_json:`/`header:` url options parse
/// to generated JSON / header strings.
#[test]
fn post_json_and_header_options_parse() {
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/\", post_json: {\n      query: \"q { f(tags: [\\\"x\\\"]) }\",\n      n: 3,\n      flag: true,\n    }\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.post_json.as_deref(),
        Some("{\"query\":\"q { f(tags: [\\\"x\\\"]) }\",\"n\":3,\"flag\":true}")
    );
    assert!(!spec.url_options);
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/\", post_json: {}\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.post_json.as_deref(), Some("{}"));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/\", header: \"RSC: 1\"\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.header, Some(vec!["RSC: 1".to_string()]));
    // r460: UA symbols, referer/cookies as headers,
    // %Q urls, symbol block-pass strategies.
    use super::super::formula::{JsonShape, LivecheckUrl, PageShape};
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/\", user_agent: :curl, referer: \"https://example.com/\", cookies: { \"a\" => \"1\" }\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.user_agent.as_deref(), Some(":curl"));
    assert_eq!(
        spec.header,
        Some(vec![
            "Referer: https://example.com/".to_string(),
            "Cookie: a=1".to_string()
        ])
    );
    assert!(!spec.url_options);
    let spec = parse_livecheck_block(
        "  livecheck do\n    url %Q(https://example.com/?q=[{\"a\":1}])\n    strategy :json, &:to_s\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.url,
        Some(LivecheckUrl::Literal(
            "https://example.com/?q=[{\"a\":1}]".to_string()
        ))
    );
    assert!(matches!(
        spec.json_block.map(|b| b.shape),
        Some(JsonShape::RootToS)
    ));
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/version\"\n    strategy :page_match, &:strip\n  end\n",
    )
    .unwrap();
    assert!(matches!(
        spec.page_block.map(|b| b.shape),
        Some(PageShape::PageStrip)
    ));
}

/// r449: `:header_match` blocks reduce (guarded
/// match template, scan flatten, csv-optional).
#[test]
fn header_match_shapes_parse() {
    use super::super::formula::{HeaderShape, PagePart};
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\"\n    regex(/v(\\d+)-(\\d+)/)\n    strategy :header_match do |headers, regex|\n      match = headers[\"location\"]&.match(regex)\n      next if match.blank?\n      \"#{match[2]},#{match[1]}\"\n    end\n  end\n",
    )
    .unwrap();
    let block = spec.header_block.unwrap();
    assert_eq!(block.header, "location");
    assert_eq!(
        block.shape,
        HeaderShape::Template(vec![
            PagePart::Cap(2),
            PagePart::Lit(",".into()),
            PagePart::Cap(1),
        ])
    );
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\"\n    strategy :header_match do |headers|\n      headers[\"x-ms-meta-version\"]\n    end\n  end\n",
    )
    .unwrap();
    assert_eq!(spec.header_block.unwrap().shape, HeaderShape::Bare);
    // r461: forks with rich capture transforms.
    use super::super::formula::PageCond;
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\"\n    regex(/v(\\d+)-(\\d+)/)\n    strategy :header_match do |headers, regex|\n      match = headers[\"location\"]&.match(regex)\n      next if match.blank?\n      (match[1] == match[2].tr(\".\", \"\")) ? match[2] : \"#{match[2].split(\"\", 3).join(\".\")},#{match[1]}\"\n    end\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.header_block.unwrap().shape,
        HeaderShape::Fork {
            cond: PageCond::CapTrEq(1, 2, ".".into(), "".into()),
            then: vec![PagePart::Cap(2)],
            els: vec![
                PagePart::CapSplitJoin(2, 3, ".".into()),
                PagePart::Lit(",".into()),
                PagePart::Cap(1),
            ],
        }
    );
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/dl\"\n    regex(/v(\\d+)/)\n    strategy :header_match do |headers, regex|\n      match = headers[\"content-disposition\"]&.match(regex)\n      next if match.blank?\n      (version = match[1]).include?(\".\") ? version : version.chars.join(\".\")\n    end\n  end\n",
    )
    .unwrap();
    assert_eq!(
        spec.header_block.unwrap().shape,
        HeaderShape::Fork {
            cond: PageCond::CapIncludes(1, ".".into()),
            then: vec![PagePart::Cap(1)],
            els: vec![PagePart::CapSplitJoin(1, 0, ".".into())],
        }
    );
}

#[test]
fn a_first_matching_header_loop_requires_an_explicit_break() {
    use super::super::formula::{HeaderShape, PagePart};
    let parsed = |render: &str, stop: &str| {
        parse_livecheck_block(&format!(
            "  livecheck do\n    url \"https://example.com/dl\"\n    regex(/v(\\d+)-(\\d+)/)\n    strategy :header_match do |all_headers, regex|\n      version = nil\n      all_headers.each do |headers|\n        matched = headers[\"location\"]&.match(regex)\n        next unless matched\n        version = {render}\n{stop}      end\n      version\n    end\n  end\n"
        ))
        .unwrap()
        .header_block
    };
    for (render, parts) in [
        (
            r##""#{matched[2]},#{matched[1]}""##,
            vec![
                PagePart::Cap(2),
                PagePart::Lit(",".into()),
                PagePart::Cap(1),
            ],
        ),
        ("matched[1]", vec![PagePart::Cap(1)]),
    ] {
        let first = parsed(render, "        break\n").unwrap();
        assert_eq!(first.header, "location");
        assert_eq!(first.shape, HeaderShape::AllTemplate(parts));
        assert!(
            parsed(render, "").is_none(),
            "without a break, {render} keeps the last matching response"
        );
        assert!(
            parsed(render, "        breakend\n").is_none(),
            "another identifier cannot stand in for the break statement"
        );
    }
}

/// r450: bare version chains as `post_json:`
/// values (`version.major_minor_patch.to_s` mid
/// hash), `version.tr(...)` arg methods in cask
/// url interpolation, and the json tail-capture
/// local shapes.
#[test]
fn post_json_bare_version_chain() {
    let pj = parse_post_json(
        "{ ApplicationName: \"express-mac\", ClientInfo: { OperatingSystemType: \"mac\" }, CurrentVersion: version.major_minor_patch.to_s }",
        &|s: &str| Some(s.to_string()),
    );
    assert_eq!(
        pj.as_deref(),
        Some(
            "{\"ApplicationName\":\"express-mac\",\"ClientInfo\":{\"OperatingSystemType\":\"mac\"},\"CurrentVersion\":\"#{version.major_minor_patch}\"}"
        )
    );
    // A chain the token grammar rejects fails closed.
    assert_eq!(
        parse_post_json("{ v: version.frobnicate }", &|s: &str| Some(s.to_string())),
        None
    );
}

#[test]
fn cask_url_version_tr_interpolation() {
    use super::super::formula::cask_version_token;
    assert_eq!(
        cask_version_token("160.0,1", "version.tr(\",\", \"-\")").as_deref(),
        Some("160.0-1")
    );
    assert_eq!(
        cask_version_token("1.2.3,45", "version.csv.first.tr(\".\", \"_\")").as_deref(),
        Some("1_2_3")
    );
    assert_eq!(
        cask_version_token("v1.2", "version.delete_prefix(\"v\")").as_deref(),
        Some("1.2")
    );
    // A terminal character index
    // (`version.csv.second[0]`, macgamestore).
    assert_eq!(
        cask_version_token("5.8.3,7055", "version.csv.second[0]").as_deref(),
        Some("7")
    );
    assert_eq!(
        cask_version_token("5.8.3,7055", "version.csv.second[-1]").as_deref(),
        Some("5")
    );
    // Ranges/negation stay unmodeled.
    assert_eq!(
        cask_version_token("a1", "version.tr(\"a-z\", \"A-Z\")"),
        None
    );
    assert_eq!(cask_version_token("a1", "version.tr(\"^a\", \"b\")"), None);
    let text = "cask \"demo\" do\n  version \"160.0,1\"\n  url \"https://example.com/#{version.tr(\",\", \"-\")}/app.dmg\"\nend\n";
    let c = parse_cask_rb_as_data(text, "demo");
    assert_eq!(c.url, "https://example.com/160.0-1/app.dmg");
}

#[test]
fn json_capture_with_tr_tail_parses() {
    use super::super::formula::{JsonLocal, JsonRender, JsonShape, JsonTail, JsonValue};
    let spec = parse_livecheck_block(
        "  livecheck do\n    url \"https://example.com/latest\"\n    regex(/^v?(\\d+(?:[.-]\\d+)+)$/i)\n    strategy :json do |json, regex|\n      json[\"tag_name\"]&.[](regex, 1)&.tr(\"-\", \",\")\n    end\n  end\n",
    )
    .unwrap();
    let Some(JsonShape::Template(tmpl)) = spec.json_block.map(|b| b.shape) else {
        panic!("expected template shape");
    };
    assert!(matches!(tmpl.locals[0], JsonLocal::DigCapture(_)));
    assert_eq!(
        tmpl.render,
        JsonRender::Value(
            JsonValue::Local(0),
            JsonTail::Tr("-".to_string(), ",".to_string())
        )
    );
}

/// r455: nested cask locals (`os` interpolating
/// `arch`) resolve at spec load; bare conditional
/// and `|| version` chains resolve in cask urls.
#[test]
fn cask_url_nested_locals_and_conditionals() {
    let text = "cask \"demo\" do\n  arch arm: \"Silicon\", intel: \"Intel\"\n  os macos: \"_macOS_#{arch}.dmg\", linux: \"_#{arch}.AppImage\"\n  version \"2026.11.10\"\n  url \"https://example.com/v#{version.csv.first.dots_to_hyphens}/App_#{version.csv.second || version}#{os}\"\n  livecheck do\n    url :url\n    regex(/App[._-]v?(\\d+(?:\\.\\d+)+)#{os}/i)\n    strategy :github_latest do |json, regex|\n      match = json[\"tag_name\"]&.match(regex)\n      next unless match\n\n      match[1]\n    end\n  end\nend\n";
    let c = parse_cask_rb_as_data(text, "demo");
    // `arch` and `os` are this host's.
    let arch = if cfg!(target_arch = "x86_64") {
        "Intel"
    } else {
        "Silicon"
    };
    let os = if cfg!(target_os = "macos") {
        format!("_macOS_{arch}.dmg")
    } else {
        format!("_{arch}.AppImage")
    };
    assert_eq!(
        c.url,
        format!("https://example.com/v2026-11-10/App_2026.11.10{os}")
    );
    let spec = parse_livecheck_block(text).unwrap();
    assert_eq!(spec.regex, Some(format!(r"App[._-]v?(\d+(?:\.\d+)+){os}")));
    let text2 = "cask \"demo\" do\n  version \"1.1.0.6161,-final\"\n  url \"https://example.com/v#{version.csv.first}#{version.csv.second if version.csv.second}/App.dmg\"\nend\n";
    assert_eq!(
        parse_cask_rb_as_data(text2, "demo").url,
        "https://example.com/v1.1.0.6161-final/App.dmg"
    );
    let text3 = "cask \"demo\" do\n  version \"1.1.0\"\n  url \"https://example.com/v#{version.csv.first}#{version.csv.second if version.csv.second}/App.dmg\"\nend\n";
    assert_eq!(
        parse_cask_rb_as_data(text3, "demo").url,
        "https://example.com/v1.1.0/App.dmg"
    );
}
